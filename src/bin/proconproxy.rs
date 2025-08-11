use procon::device::ProController;


fn main() -> anyhow::Result<()> {
    env_logger::init();

    log::info!("ProCon Proxy");

    let mut parser = ProController::connect()?;

    parser.start_capture()?;

    Ok(())
}
