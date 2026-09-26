//! Python bindings (module `gameplay_data`), built with maturin.
//!
//! Columns come and go as C-contiguous numpy arrays; buttons as bool arrays
//! with one column per [`BUTTONS`] entry. Structured data (the session
//! description, labels, calibrations) comes and goes as JSON text of this
//! crate's types, for `json.loads` / `json.dumps` on the Python side.

use crate::align::{self, FrameActions};
use crate::calibration::read_calibrations;
use crate::controller::{BUTTONS, ControllerLog, STICK_NAMES};
use crate::labels::{self, Label};
use crate::session::SessionInfo;
use alloc::string::String;
use alloc::vec::Vec;
use numpy::{
    IntoPyArray, PyArrayMethods, PyReadonlyArray1, PyReadonlyArray2, PyUntypedArrayMethods,
};
use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;
use pyo3::types::PyDict;
use std::path::PathBuf;

/// A contiguous array's elements, or a Python error naming it
fn slice<'a, T: numpy::Element, D: numpy::ndarray::Dimension>(
    array: &'a numpy::PyReadonlyArray<'_, T, D>,
    name: &str,
) -> PyResult<&'a [T]> {
    array
        .as_slice()
        .map_err(|_| PyValueError::new_err(alloc::format!("{name} must be C-contiguous")))
}

/// Rows of `N` from a contiguous `[rows, N]` array
fn rows<T: numpy::Element + Copy, const N: usize>(
    array: &PyReadonlyArray2<'_, T>,
    name: &str,
) -> PyResult<Vec<[T; N]>> {
    if array.shape()[1] != N {
        return Err(PyValueError::new_err(alloc::format!(
            "{name} must have {N} columns"
        )));
    }
    Ok(slice(array, name)?.as_chunks::<N>().0.to_vec())
}

/// Button masks from a bool `[rows, 22]` array
fn masks(array: &PyReadonlyArray2<'_, bool>, name: &str) -> PyResult<Vec<u32>> {
    Ok(rows::<bool, { BUTTONS.len() }>(array, name)?
        .iter()
        .map(|row| {
            row.iter()
                .enumerate()
                .fold(0, |bits, (i, pressed)| bits | u32::from(*pressed) << i)
        })
        .collect())
}

/// A `[rows, N]` numpy array
fn array2<'py, T: numpy::Element + Copy, const N: usize>(
    py: Python<'py>,
    rows: &[[T; N]],
) -> PyResult<Bound<'py, PyAny>> {
    let flat: Vec<T> = rows.iter().flatten().copied().collect();
    Ok(flat.into_pyarray(py).reshape([rows.len(), N])?.into_any())
}

/// A bool `[rows, 22]` numpy array from button masks
fn buttons_array<'py>(py: Python<'py>, masks: &[u32]) -> PyResult<Bound<'py, PyAny>> {
    let rows: Vec<[bool; BUTTONS.len()]> = masks
        .iter()
        .map(|bits| core::array::from_fn(|i| bits >> i & 1 == 1))
        .collect();
    array2(py, &rows)
}

/// Parse `controller.bin` into a dict of columns: `time_ms` int64,
/// `seq` uint32, `report_id` uint8, `forward_us` float32 (NaN if unknown),
/// `buttons` bool [N, 22], `sticks` uint16 [N, 4], `imu_time_ms` and
/// `imu_dt_ms` float64, `gyro` (deg/s) and `accel` (g) float32 [S, 3], and
/// `dropped`
#[pyfunction]
fn read_controller(py: Python<'_>, path: PathBuf) -> PyResult<Bound<'_, PyDict>> {
    let log = ControllerLog::read(&path)?;
    let dict = PyDict::new(py);
    let time_ms: Vec<i64> = log.time_ms.iter().map(|t| *t as i64).collect();
    let forward_us: Vec<f32> = log
        .forward_us
        .iter()
        .map(|us| if *us == 0 { f32::NAN } else { f32::from(*us) })
        .collect();
    dict.set_item("time_ms", time_ms.into_pyarray(py))?;
    dict.set_item("seq", log.seq.clone().into_pyarray(py))?;
    dict.set_item("report_id", log.report_id.clone().into_pyarray(py))?;
    dict.set_item("forward_us", forward_us.into_pyarray(py))?;
    dict.set_item("buttons", buttons_array(py, &log.buttons)?)?;
    dict.set_item("sticks", array2(py, &log.sticks)?)?;
    dict.set_item("imu_time_ms", log.imu_time_ms.clone().into_pyarray(py))?;
    dict.set_item("imu_dt_ms", log.imu_dt_ms.clone().into_pyarray(py))?;
    dict.set_item("gyro", array2(py, &log.gyro)?)?;
    dict.set_item("accel", array2(py, &log.accel)?)?;
    dict.set_item("dropped", log.dropped)?;
    Ok(dict)
}

/// Aggregate controller columns (as from `read_controller`) over frames
/// starting at `frame_time_ms` and lasting `frame_ms`; returns a dict of
/// `time_ms`, `mask`, `buttons`, `buttons_held` (bool [F, 22]), `sticks`
/// (float32 [F, 4]), `gyro` (degrees) and `accel` (g, float32 [F, 3])
#[pyfunction(name = "align")]
#[allow(clippy::too_many_arguments)]
fn align_frames<'py>(
    py: Python<'py>,
    time_ms: PyReadonlyArray1<'py, i64>,
    buttons: PyReadonlyArray2<'py, bool>,
    sticks: PyReadonlyArray2<'py, u16>,
    imu_time_ms: PyReadonlyArray1<'py, f64>,
    imu_dt_ms: PyReadonlyArray1<'py, f64>,
    gyro: PyReadonlyArray2<'py, f32>,
    accel: PyReadonlyArray2<'py, f32>,
    frame_time_ms: PyReadonlyArray1<'py, f64>,
    frame_ms: f64,
    controller_shift_ms: f64,
) -> PyResult<Bound<'py, PyDict>> {
    let log = ControllerLog {
        time_ms: slice(&time_ms, "time_ms")?
            .iter()
            .map(|t| *t as u64)
            .collect(),
        buttons: masks(&buttons, "buttons")?,
        sticks: rows(&sticks, "sticks")?,
        imu_time_ms: slice(&imu_time_ms, "imu_time_ms")?.to_vec(),
        imu_dt_ms: slice(&imu_dt_ms, "imu_dt_ms")?.to_vec(),
        gyro: rows(&gyro, "gyro")?,
        accel: rows(&accel, "accel")?,
        ..ControllerLog::default()
    };
    let frames = slice(&frame_time_ms, "frame_time_ms")?;
    let actions = align::align(&log, frames, frame_ms, controller_shift_ms);
    let dict = PyDict::new(py);
    dict.set_item("time_ms", actions.time_ms.into_pyarray(py))?;
    dict.set_item("mask", actions.mask.into_pyarray(py))?;
    dict.set_item("buttons", buttons_array(py, &actions.buttons)?)?;
    dict.set_item("buttons_held", buttons_array(py, &actions.buttons_held)?)?;
    dict.set_item("sticks", array2(py, &actions.sticks)?)?;
    dict.set_item("gyro", array2(py, &actions.gyro)?)?;
    dict.set_item("accel", array2(py, &actions.accel)?)?;
    Ok(dict)
}

/// On-screen times (float64) of a constant-rate segment's `count` frames
#[pyfunction]
fn constant_rate_times(
    py: Python<'_>,
    start_unix_ms: u64,
    count: usize,
    fps: f64,
    video_delay_ms: f64,
) -> Bound<'_, PyAny> {
    align::constant_rate_times(start_unix_ms, count, fps, video_delay_ms)
        .into_pyarray(py)
        .into_any()
}

/// On-screen times (float64) of a variable-rate segment's frames, from
/// their sorted presentation times in ms
#[pyfunction]
fn variable_rate_times<'py>(
    py: Python<'py>,
    start_unix_ms: u64,
    pts_ms: PyReadonlyArray1<'py, f64>,
    video_delay_ms: f64,
) -> PyResult<Bound<'py, PyAny>> {
    let pts_ms = slice(&pts_ms, "pts_ms")?;
    Ok(
        align::variable_rate_times(start_unix_ms, pts_ms, video_delay_ms)
            .into_pyarray(py)
            .into_any(),
    )
}

/// A session folder's `session.json`, checked and written back as JSON
#[pyfunction]
fn read_session(path: PathBuf) -> PyResult<String> {
    Ok(serde_json::to_string(&SessionInfo::read(&path)?).map_err(anyhow::Error::from)?)
}

/// JSON array of the labels of frames `first_frame ..` given the per-frame
/// `mask` [F], `buttons` bool [F, 22], `sticks` float32 [F, 4] and `gyro`
/// float32 [F, 3] of those frames
#[pyfunction]
fn frame_labels(
    mask: PyReadonlyArray1<'_, bool>,
    buttons: PyReadonlyArray2<'_, bool>,
    sticks: PyReadonlyArray2<'_, f32>,
    gyro: PyReadonlyArray2<'_, f32>,
    first_frame: u64,
) -> PyResult<String> {
    let actions = FrameActions {
        mask: slice(&mask, "mask")?.to_vec(),
        buttons: masks(&buttons, "buttons")?,
        sticks: rows(&sticks, "sticks")?,
        gyro: rows(&gyro, "gyro")?,
        ..FrameActions::default()
    };
    let mut labels = labels::frame_labels(&actions, 0..actions.mask.len());
    for label in &mut labels {
        label.frame += first_frame;
    }
    Ok(serde_json::to_string(&labels).map_err(anyhow::Error::from)?)
}

/// Write labels given as a JSON array, one object per line
#[pyfunction]
fn write_labels(path: PathBuf, labels: &str) -> PyResult<()> {
    let labels: Vec<Label> = serde_json::from_str(labels).map_err(anyhow::Error::from)?;
    Ok(labels::write_labels(&path, &labels)?)
}

/// A labels file as a JSON array
#[pyfunction]
fn read_labels(path: PathBuf) -> PyResult<String> {
    Ok(serde_json::to_string(&labels::read_labels(&path)?).map_err(anyhow::Error::from)?)
}

/// A calibration file as a JSON object (empty if the file is missing)
#[pyfunction]
fn read_calibration(path: PathBuf) -> PyResult<String> {
    Ok(serde_json::to_string(&read_calibrations(&path)?).map_err(anyhow::Error::from)?)
}

/// The delay to apply to a session: its measured one if confidence is
/// high or medium, else None
#[pyfunction]
fn calibrated_delay_ms(path: PathBuf, session: &str) -> PyResult<Option<f64>> {
    Ok(read_calibrations(&path)?
        .get(session)
        .and_then(|c| c.applied_delay_ms()))
}

#[pymodule]
fn gameplay_data(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add("BUTTON_NAMES", BUTTONS.map(|(name, _, _)| name))?;
    m.add("STICK_NAMES", STICK_NAMES)?;
    m.add("FRAME_SIZE", crate::frame::FRAME_SIZE)?;
    m.add_function(wrap_pyfunction!(read_controller, m)?)?;
    m.add_function(wrap_pyfunction!(align_frames, m)?)?;
    m.add_function(wrap_pyfunction!(constant_rate_times, m)?)?;
    m.add_function(wrap_pyfunction!(variable_rate_times, m)?)?;
    m.add_function(wrap_pyfunction!(read_session, m)?)?;
    m.add_function(wrap_pyfunction!(frame_labels, m)?)?;
    m.add_function(wrap_pyfunction!(write_labels, m)?)?;
    m.add_function(wrap_pyfunction!(read_labels, m)?)?;
    m.add_function(wrap_pyfunction!(read_calibration, m)?)?;
    m.add_function(wrap_pyfunction!(calibrated_delay_ms, m)?)?;
    Ok(())
}
