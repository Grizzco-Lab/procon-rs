use anyhow::Result;

/// Trait for dumping Pro Controller input data
pub trait Dumper {
    fn dump(&mut self, data: &[u8]) -> Result<()>;
    fn flush(&mut self) -> Result<()>;
}

/// File-based dumper that writes data to a file
pub struct FileDumper {
    // TODO: Add file handle or path
}

impl FileDumper {
    pub fn new(_file_path: &str) -> Result<Self> {
        // TODO: Implement file opening
        log::info!("FileDumper created (placeholder)");
        Ok(FileDumper {})
    }
}

impl Dumper for FileDumper {
    fn dump(&mut self, data: &[u8]) -> Result<()> {
        // TODO: Implement actual dumping
        log::trace!("Dumping {} bytes (placeholder)", data.len());
        Ok(())
    }

    fn flush(&mut self) -> Result<()> {
        // TODO: Implement flushing
        Ok(())
    }
}

/// No-op dumper that discards all data
pub struct NullDumper;

impl NullDumper {
    pub fn new() -> Self {
        NullDumper
    }
}

impl Dumper for NullDumper {
    fn dump(&mut self, _data: &[u8]) -> Result<()> {
        Ok(())
    }

    fn flush(&mut self) -> Result<()> {
        Ok(())
    }
}
