use crate::clock::TimedTick;
use crate::Result;

pub trait StreamProcessor: Send + 'static {
    fn process(&mut self, tick: TimedTick) -> Result<()>;

    fn on_start(&mut self) -> Result<()> {
        Ok(())
    }

    fn on_stop(&mut self) -> Result<()> {
        Ok(())
    }
}
