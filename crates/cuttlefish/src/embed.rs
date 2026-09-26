//! Text embeddings.
//!
//! [`E5Embedder`] runs `intfloat/multilingual-e5-small` (MIT license, 118M
//! parameters, 384 dimensions, about 100 languages including Japanese and
//! Chinese) with candle on the CPU, or on CUDA with the `cuda` feature. The
//! model files are downloaded once from the Hugging Face hub into the data
//! folder. [`HashEmbedder`] is a deterministic stand-in for tests.

use crate::crawl;
use alloc::string::String;
use alloc::vec::Vec;
use anyhow::{Context, Result, anyhow};
use candle_core::{DType, Device, Tensor};
use candle_nn::VarBuilder;
use candle_transformers::models::bert::{BertModel, Config};
use std::path::Path;
use tokenizers::{PaddingParams, Tokenizer, TruncationParams};

/// What a text is: E5 models embed questions and passages with different
/// prefixes
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    /// A search query
    Query,
    /// A stored chunk
    Passage,
}

/// Turns texts into unit-length vectors
pub trait Embedder: Send + Sync {
    /// Name stored with the index; vectors of different embedders never mix
    fn name(&self) -> &str;
    /// Vector length
    fn dim(&self) -> usize;
    /// One vector per text, each of length [`Embedder::dim`] and norm 1
    fn embed(&self, texts: &[&str], role: Role) -> Result<Vec<Vec<f32>>>;
}

/// `intfloat/multilingual-e5-small` on candle
pub struct E5Embedder {
    model: BertModel,
    tokenizer: Tokenizer,
    device: Device,
}

impl E5Embedder {
    /// Hugging Face repository
    pub const REPO: &str = "intfloat/multilingual-e5-small";
    /// Pinned revision, so the index never mixes model versions
    pub const REVISION: &str = "614241f622f53c4eeff9890bdc4f31cfecc418b3";
    /// Files the model needs
    const FILES: [&str; 3] = ["config.json", "tokenizer.json", "model.safetensors"];
    /// Texts per forward pass
    const BATCH: usize = 16;

    /// Loads the model from `models_dir`, downloading it on first use
    pub fn load(models_dir: &Path) -> Result<Self> {
        let dir = models_dir
            .join(Self::REPO.replace('/', "--"))
            .join(Self::REVISION);
        std::fs::create_dir_all(&dir)?;
        for file in Self::FILES {
            let path = dir.join(file);
            if !path.exists() {
                let url = alloc::format!(
                    "https://huggingface.co/{}/resolve/{}/{file}",
                    Self::REPO,
                    Self::REVISION
                );
                log::info!("downloading {url}");
                crawl::download(&url, &path)?;
            }
        }
        let device = Self::device()?;
        let config: Config =
            serde_json::from_str(&std::fs::read_to_string(dir.join("config.json"))?)?;
        // SAFETY: the file is only read, and not changed while mapped
        let vb = unsafe {
            VarBuilder::from_mmaped_safetensors(
                &[dir.join("model.safetensors")],
                DType::F32,
                &device,
            )?
        };
        let model = BertModel::load(vb, &config)?;
        let mut tokenizer =
            Tokenizer::from_file(dir.join("tokenizer.json")).map_err(|e| anyhow!(e))?;
        tokenizer.with_padding(Some(PaddingParams::default()));
        tokenizer
            .with_truncation(Some(TruncationParams {
                max_length: 512,
                ..Default::default()
            }))
            .map_err(|e| anyhow!(e))?;
        Ok(E5Embedder {
            model,
            tokenizer,
            device,
        })
    }

    #[cfg(feature = "cuda")]
    fn device() -> Result<Device> {
        Ok(Device::cuda_if_available(0)?)
    }

    #[cfg(not(feature = "cuda"))]
    fn device() -> Result<Device> {
        Ok(Device::Cpu)
    }

    fn embed_batch(&self, texts: Vec<String>) -> Result<Vec<Vec<f32>>> {
        let enc = self
            .tokenizer
            .encode_batch(texts, true)
            .map_err(|e| anyhow!(e))?;
        let ids: Vec<Tensor> = enc
            .iter()
            .map(|e| Tensor::new(e.get_ids(), &self.device))
            .collect::<candle_core::Result<_>>()?;
        let masks: Vec<Tensor> = enc
            .iter()
            .map(|e| Tensor::new(e.get_attention_mask(), &self.device))
            .collect::<candle_core::Result<_>>()?;
        let ids = Tensor::stack(&ids, 0)?;
        let mask = Tensor::stack(&masks, 0)?;
        let types = ids.zeros_like()?;
        let hidden = self.model.forward(&ids, &types, Some(&mask))?;
        // Mean over real tokens, then unit length
        let mask = mask.to_dtype(DType::F32)?.unsqueeze(2)?;
        let summed = hidden.broadcast_mul(&mask)?.sum(1)?;
        let counts = mask.sum(1)?;
        let mean = summed.broadcast_div(&counts)?;
        let norm = mean.sqr()?.sum_keepdim(1)?.sqrt()?;
        Ok(mean.broadcast_div(&norm)?.to_vec2()?)
    }
}

impl Embedder for E5Embedder {
    fn name(&self) -> &str {
        "multilingual-e5-small@614241f"
    }

    fn dim(&self) -> usize {
        384
    }

    fn embed(&self, texts: &[&str], role: Role) -> Result<Vec<Vec<f32>>> {
        let prefix = match role {
            Role::Query => "query: ",
            Role::Passage => "passage: ",
        };
        let mut out = Vec::with_capacity(texts.len());
        for batch in texts.chunks(Self::BATCH) {
            let batch = batch
                .iter()
                .map(|t| alloc::format!("{prefix}{t}"))
                .collect();
            out.extend(self.embed_batch(batch).context("embedding")?);
        }
        Ok(out)
    }
}

/// Deterministic bag-of-words embedder: each lowercase word (or CJK
/// character) adds to a hashed dimension. No semantics, for tests only.
pub struct HashEmbedder {
    /// Vector length
    pub dim: usize,
}

impl Embedder for HashEmbedder {
    fn name(&self) -> &str {
        "hash"
    }

    fn dim(&self) -> usize {
        self.dim
    }

    fn embed(&self, texts: &[&str], _role: Role) -> Result<Vec<Vec<f32>>> {
        Ok(texts
            .iter()
            .map(|t| {
                let mut v = alloc::vec![0.0f32; self.dim];
                let lower = t.to_lowercase();
                let words = lower
                    .split(|c: char| !c.is_alphanumeric())
                    .filter(|w| !w.is_empty());
                for w in words {
                    let h = crate::doc::doc_id(w);
                    let i = u64::from_str_radix(&h, 16).unwrap_or(0) as usize % self.dim;
                    v[i] += 1.0;
                }
                let n = v.iter().map(|x| x * x).sum::<f32>().sqrt();
                if n > 0.0 {
                    v.iter_mut().for_each(|x| *x /= n);
                }
                v
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_embedder_is_deterministic_and_unit() {
        let e = HashEmbedder { dim: 64 };
        let a = e.embed(&["Egg flow", "egg FLOW"], Role::Passage).unwrap();
        assert_eq!(a[0], a[1]);
        let n: f32 = a[0].iter().map(|x| x * x).sum();
        assert!((n - 1.0).abs() < 1e-5);
    }
}
