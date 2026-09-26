//! The YOLOv8 detection network in candle.
//!
//! Adapted from candle's `yolo-v8` example (candle-examples 0.11,
//! MIT OR Apache-2.0), which follows the tinygrad port of the architecture;
//! the pose head and tracing spans are left out. Tensor names match the
//! `lmz/candle-yolo-v8` safetensors files, so the same code loads those COCO
//! weights or our own with another class count.
//!
//! The architecture code is permissively licensed; the pretrained COCO
//! weights are converted from Ultralytics, whose models are AGPL-3.0 (see
//! the crate README).

use candle_core::{D, DType, IndexOp, Module, Result, Tensor};
use candle_nn::{Conv2d, Conv2dConfig, VarBuilder, batch_norm, conv2d, conv2d_no_bias};

/// Depth, width and ratio multipliers of a model size
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Multiples {
    depth: f64,
    width: f64,
    ratio: f64,
}

impl Multiples {
    /// The multipliers of size `n`, `s`, `m`, `l` or `x`
    pub fn of(size: char) -> Option<Self> {
        let (depth, width, ratio) = match size {
            'n' => (0.33, 0.25, 2.0),
            's' => (0.33, 0.50, 2.0),
            'm' => (0.67, 0.75, 1.5),
            'l' => (1.00, 1.00, 1.0),
            'x' => (1.00, 1.25, 1.0),
            _ => return None,
        };
        Some(Self {
            depth,
            width,
            ratio,
        })
    }

    fn filters(&self) -> (usize, usize, usize) {
        let f1 = (256. * self.width) as usize;
        let f2 = (512. * self.width) as usize;
        let f3 = (512. * self.width * self.ratio) as usize;
        (f1, f2, f3)
    }
}

/// Convolution with its batch norm folded in, then SiLU
#[derive(Debug)]
struct ConvBlock {
    conv: Conv2d,
}

impl ConvBlock {
    fn load(vb: VarBuilder, c1: usize, c2: usize, k: usize, stride: usize) -> Result<Self> {
        let cfg = Conv2dConfig {
            padding: k / 2,
            stride,
            groups: 1,
            dilation: 1,
            cudnn_fwd_algo: None,
        };
        let bn = batch_norm(c2, 1e-3, vb.pp("bn"))?;
        let conv = conv2d_no_bias(c1, c2, k, cfg, vb.pp("conv"))?.absorb_bn(&bn)?;
        Ok(Self { conv })
    }
}

impl Module for ConvBlock {
    fn forward(&self, xs: &Tensor) -> Result<Tensor> {
        candle_nn::ops::silu(&self.conv.forward(xs)?)
    }
}

/// Two 3x3 convolutions with an optional residual
#[derive(Debug)]
struct Bottleneck {
    cv1: ConvBlock,
    cv2: ConvBlock,
    residual: bool,
}

impl Bottleneck {
    fn load(vb: VarBuilder, c1: usize, c2: usize, shortcut: bool) -> Result<Self> {
        let cv1 = ConvBlock::load(vb.pp("cv1"), c1, c2, 3, 1)?;
        let cv2 = ConvBlock::load(vb.pp("cv2"), c2, c2, 3, 1)?;
        Ok(Self {
            cv1,
            cv2,
            residual: c1 == c2 && shortcut,
        })
    }
}

impl Module for Bottleneck {
    fn forward(&self, xs: &Tensor) -> Result<Tensor> {
        let ys = self.cv2.forward(&self.cv1.forward(xs)?)?;
        if self.residual { xs + ys } else { Ok(ys) }
    }
}

/// CSP block with two convolutions and `n` bottlenecks
#[derive(Debug)]
struct C2f {
    cv1: ConvBlock,
    cv2: ConvBlock,
    bottleneck: Vec<Bottleneck>,
}

impl C2f {
    fn load(vb: VarBuilder, c1: usize, c2: usize, n: usize, shortcut: bool) -> Result<Self> {
        let c = c2 / 2;
        let cv1 = ConvBlock::load(vb.pp("cv1"), c1, 2 * c, 1, 1)?;
        let cv2 = ConvBlock::load(vb.pp("cv2"), (2 + n) * c, c2, 1, 1)?;
        let bottleneck = (0..n)
            .map(|i| Bottleneck::load(vb.pp(format!("bottleneck.{i}")), c, c, shortcut))
            .collect::<Result<_>>()?;
        Ok(Self {
            cv1,
            cv2,
            bottleneck,
        })
    }
}

impl Module for C2f {
    fn forward(&self, xs: &Tensor) -> Result<Tensor> {
        let mut ys = self.cv1.forward(xs)?.chunk(2, 1)?;
        for m in &self.bottleneck {
            ys.push(m.forward(ys.last().unwrap())?)
        }
        self.cv2.forward(&Tensor::cat(&ys, 1)?)
    }
}

/// Spatial pyramid pooling, fast: three chained max pools
#[derive(Debug)]
struct Sppf {
    cv1: ConvBlock,
    cv2: ConvBlock,
    k: usize,
}

impl Sppf {
    fn load(vb: VarBuilder, c1: usize, c2: usize, k: usize) -> Result<Self> {
        let c = c1 / 2;
        let cv1 = ConvBlock::load(vb.pp("cv1"), c1, c, 1, 1)?;
        let cv2 = ConvBlock::load(vb.pp("cv2"), c * 4, c2, 1, 1)?;
        Ok(Self { cv1, cv2, k })
    }
}

impl Module for Sppf {
    fn forward(&self, xs: &Tensor) -> Result<Tensor> {
        let pool = |xs: &Tensor| {
            xs.pad_with_zeros(2, self.k / 2, self.k / 2)?
                .pad_with_zeros(3, self.k / 2, self.k / 2)?
                .max_pool2d_with_stride(self.k, 1)
        };
        let xs = self.cv1.forward(xs)?;
        let xs2 = pool(&xs)?;
        let xs3 = pool(&xs2)?;
        let xs4 = pool(&xs3)?;
        self.cv2.forward(&Tensor::cat(&[&xs, &xs2, &xs3, &xs4], 1)?)
    }
}

/// Distribution focal loss decoding: the expected box side distance
#[derive(Debug)]
struct Dfl {
    conv: Conv2d,
    bins: usize,
}

impl Dfl {
    fn load(vb: VarBuilder, bins: usize) -> Result<Self> {
        let conv = conv2d_no_bias(bins, 1, 1, Default::default(), vb.pp("conv"))?;
        Ok(Self { conv, bins })
    }
}

impl Module for Dfl {
    fn forward(&self, xs: &Tensor) -> Result<Tensor> {
        let (b, _, anchors) = xs.dims3()?;
        let xs = xs.reshape((b, 4, self.bins, anchors))?.transpose(2, 1)?;
        let xs = candle_nn::ops::softmax(&xs, 1)?;
        self.conv.forward(&xs)?.reshape((b, 4, anchors))
    }
}

/// The backbone
#[derive(Debug)]
struct DarkNet {
    b1_0: ConvBlock,
    b1_1: ConvBlock,
    b2_0: C2f,
    b2_1: ConvBlock,
    b2_2: C2f,
    b3_0: ConvBlock,
    b3_1: C2f,
    b4_0: ConvBlock,
    b4_1: C2f,
    b5: Sppf,
}

impl DarkNet {
    fn load(vb: VarBuilder, m: Multiples) -> Result<Self> {
        let (w, r, d) = (m.width, m.ratio, m.depth);
        let c = |n: f64| (n * w) as usize;
        let n = |k: f64| (k * d).round() as usize;
        Ok(Self {
            b1_0: ConvBlock::load(vb.pp("b1.0"), 3, c(64.), 3, 2)?,
            b1_1: ConvBlock::load(vb.pp("b1.1"), c(64.), c(128.), 3, 2)?,
            b2_0: C2f::load(vb.pp("b2.0"), c(128.), c(128.), n(3.), true)?,
            b2_1: ConvBlock::load(vb.pp("b2.1"), c(128.), c(256.), 3, 2)?,
            b2_2: C2f::load(vb.pp("b2.2"), c(256.), c(256.), n(6.), true)?,
            b3_0: ConvBlock::load(vb.pp("b3.0"), c(256.), c(512.), 3, 2)?,
            b3_1: C2f::load(vb.pp("b3.1"), c(512.), c(512.), n(6.), true)?,
            b4_0: ConvBlock::load(vb.pp("b4.0"), c(512.), c(512. * r), 3, 2)?,
            b4_1: C2f::load(vb.pp("b4.1"), c(512. * r), c(512. * r), n(3.), true)?,
            b5: Sppf::load(vb.pp("b5.0"), c(512. * r), c(512. * r), 5)?,
        })
    }

    fn forward(&self, xs: &Tensor) -> Result<(Tensor, Tensor, Tensor)> {
        let x1 = self.b1_1.forward(&self.b1_0.forward(xs)?)?;
        let x2 = self
            .b2_2
            .forward(&self.b2_1.forward(&self.b2_0.forward(&x1)?)?)?;
        let x3 = self.b3_1.forward(&self.b3_0.forward(&x2)?)?;
        let x4 = self.b4_1.forward(&self.b4_0.forward(&x3)?)?;
        let x5 = self.b5.forward(&x4)?;
        Ok((x2, x3, x5))
    }
}

/// The feature pyramid between backbone and head
#[derive(Debug)]
struct Neck {
    n1: C2f,
    n2: C2f,
    n3: ConvBlock,
    n4: C2f,
    n5: ConvBlock,
    n6: C2f,
}

/// Nearest-neighbour upsampling by 2
fn upsample(xs: &Tensor) -> Result<Tensor> {
    let (_, _, h, w) = xs.dims4()?;
    xs.upsample_nearest2d(2 * h, 2 * w)
}

impl Neck {
    fn load(vb: VarBuilder, m: Multiples) -> Result<Self> {
        let (w, r, d) = (m.width, m.ratio, m.depth);
        let c = |n: f64| (n * w) as usize;
        let n = (3. * d).round() as usize;
        Ok(Self {
            n1: C2f::load(vb.pp("n1"), c(512. * (1. + r)), c(512.), n, false)?,
            n2: C2f::load(vb.pp("n2"), c(768.), c(256.), n, false)?,
            n3: ConvBlock::load(vb.pp("n3"), c(256.), c(256.), 3, 2)?,
            n4: C2f::load(vb.pp("n4"), c(768.), c(512.), n, false)?,
            n5: ConvBlock::load(vb.pp("n5"), c(512.), c(512.), 3, 2)?,
            n6: C2f::load(vb.pp("n6"), c(512. * (1. + r)), c(512. * r), n, false)?,
        })
    }

    fn forward(&self, p3: &Tensor, p4: &Tensor, p5: &Tensor) -> Result<(Tensor, Tensor, Tensor)> {
        let x = self.n1.forward(&Tensor::cat(&[&upsample(p5)?, p4], 1)?)?;
        let h1 = self.n2.forward(&Tensor::cat(&[&upsample(&x)?, p3], 1)?)?;
        let h2 = self
            .n4
            .forward(&Tensor::cat(&[&self.n3.forward(&h1)?, &x], 1)?)?;
        let h3 = self
            .n6
            .forward(&Tensor::cat(&[&self.n5.forward(&h2)?, p5], 1)?)?;
        Ok((h1, h2, h3))
    }
}

/// One output branch: two convolutions and a 1x1 projection
type Branch = (ConvBlock, ConvBlock, Conv2d);

fn load_branch(vb: VarBuilder, c_in: usize, c_mid: usize, c_out: usize) -> Result<Branch> {
    Ok((
        ConvBlock::load(vb.pp("0"), c_in, c_mid, 3, 1)?,
        ConvBlock::load(vb.pp("1"), c_mid, c_mid, 3, 1)?,
        conv2d(c_mid, c_out, 1, Default::default(), vb.pp("2"))?,
    ))
}

fn forward_branch(b: &Branch, xs: &Tensor) -> Result<Tensor> {
    b.2.forward(&b.1.forward(&b.0.forward(xs)?)?)
}

/// Anchor points (cell centers) and their strides for the three scales
fn make_anchors(scales: [&Tensor; 3], strides: [usize; 3]) -> Result<(Tensor, Tensor)> {
    let dev = scales[0].device();
    let mut points = vec![];
    let mut stride_values = vec![];
    for (xs, stride) in scales.into_iter().zip(strides) {
        let (_, _, h, w) = xs.dims4()?;
        let sx = (Tensor::arange(0, w as u32, dev)?.to_dtype(DType::F32)? + 0.5)?;
        let sy = (Tensor::arange(0, h as u32, dev)?.to_dtype(DType::F32)? + 0.5)?;
        let sx = sx.reshape((1, w))?.repeat((h, 1))?.flatten_all()?;
        let sy = sy.reshape((h, 1))?.repeat((1, w))?.flatten_all()?;
        points.push(Tensor::stack(&[&sx, &sy], D::Minus1)?);
        stride_values.push((Tensor::ones(h * w, DType::F32, dev)? * stride as f64)?);
    }
    let points = Tensor::cat(&points, 0)?;
    let strides = Tensor::cat(&stride_values, 0)?.unsqueeze(1)?;
    Ok((points, strides))
}

/// Box side distances around anchor points to center and size
fn dist2bbox(distance: &Tensor, anchor_points: &Tensor) -> Result<Tensor> {
    let chunks = distance.chunk(2, 1)?;
    let x1y1 = anchor_points.sub(&chunks[0])?;
    let x2y2 = anchor_points.add(&chunks[1])?;
    let c_xy = ((&x1y1 + &x2y2)? * 0.5)?;
    let wh = (&x2y2 - &x1y1)?;
    Tensor::cat(&[c_xy, wh], 1)
}

/// The detection head: box regression (`cv2`) and class scores (`cv3`)
#[derive(Debug)]
struct Head {
    dfl: Dfl,
    cv2: [Branch; 3],
    cv3: [Branch; 3],
    bins: usize,
    outputs: usize,
}

impl Head {
    fn load(vb: VarBuilder, nc: usize, filters: (usize, usize, usize)) -> Result<Self> {
        let bins = 16;
        let c1 = usize::max(filters.0, nc);
        let c2 = usize::max(filters.0 / 4, bins * 4);
        let branches = |name: &str, c_mid: usize, c_out: usize| -> Result<[Branch; 3]> {
            let load =
                |i: usize, f: usize| load_branch(vb.pp(format!("{name}.{i}")), f, c_mid, c_out);
            Ok([
                load(0, filters.0)?,
                load(1, filters.1)?,
                load(2, filters.2)?,
            ])
        };
        Ok(Self {
            dfl: Dfl::load(vb.pp("dfl"), bins)?,
            cv2: branches("cv2", c2, 4 * bins)?,
            cv3: branches("cv3", c1, nc)?,
            bins,
            outputs: nc + bins * 4,
        })
    }

    fn forward(&self, xs: [&Tensor; 3]) -> Result<Tensor> {
        let mut ys = Vec::with_capacity(3);
        for (i, x) in xs.into_iter().enumerate() {
            let boxes = forward_branch(&self.cv2[i], x)?;
            let classes = forward_branch(&self.cv3[i], x)?;
            ys.push(Tensor::cat(&[&boxes, &classes], 1)?);
        }
        let (anchors, strides) = make_anchors([&ys[0], &ys[1], &ys[2]], [8, 16, 32])?;
        let anchors = anchors.transpose(0, 1)?.unsqueeze(0)?;
        let strides = strides.transpose(0, 1)?;
        let flat = ys
            .iter()
            .map(|y| {
                let b = y.dim(0)?;
                y.reshape((b, self.outputs, y.elem_count() / (b * self.outputs)))
            })
            .collect::<Result<Vec<_>>>()?;
        let x = Tensor::cat(&flat, 2)?;
        let box_ = x.i((.., ..self.bins * 4))?;
        let cls = x.i((.., self.bins * 4..))?;
        let dbox = dist2bbox(&self.dfl.forward(&box_)?, &anchors)?.broadcast_mul(&strides)?;
        Tensor::cat(&[dbox, candle_nn::ops::sigmoid(&cls)?], 1)
    }
}

/// YOLOv8 for detection
#[derive(Debug)]
pub struct YoloV8 {
    net: DarkNet,
    fpn: Neck,
    head: Head,
}

impl YoloV8 {
    /// Load the network of size `m` with `num_classes` outputs
    pub fn load(vb: VarBuilder, m: Multiples, num_classes: usize) -> Result<Self> {
        Ok(Self {
            net: DarkNet::load(vb.pp("net"), m)?,
            fpn: Neck::load(vb.pp("fpn"), m)?,
            head: Head::load(vb.pp("head"), num_classes, m.filters())?,
        })
    }
}

impl Module for YoloV8 {
    /// `(batch, 3, h, w)` RGB in 0..1, `h` and `w` multiples of 32, to
    /// `(batch, 4 + classes, anchors)`: box center x, y, width, height in
    /// input pixels, then one sigmoid score per class
    fn forward(&self, xs: &Tensor) -> Result<Tensor> {
        let (x1, x2, x3) = self.net.forward(xs)?;
        let (x1, x2, x3) = self.fpn.forward(&x1, &x2, &x3)?;
        self.head.forward([&x1, &x2, &x3])
    }
}
