use crate::error::Result;
use crate::model::{ElementAddress, TapeBarcode};

pub trait MediumChanger {
    fn move_medium(&mut self, from: ElementAddress, to: ElementAddress) -> Result<()>;
    fn load(&mut self, slot: ElementAddress, drive: ElementAddress) -> Result<()>;
    fn unload(&mut self, drive: ElementAddress, slot: ElementAddress) -> Result<()>;
}

pub trait TapeDrive {
    fn rewind(&mut self, drive: ElementAddress) -> Result<()>;
    fn seek_filemark(&mut self, drive: ElementAddress, count: u32) -> Result<()>;
    fn write_filemark(&mut self, drive: ElementAddress) -> Result<()>;
    fn write(&mut self, drive: ElementAddress, data: &[u8]) -> Result<()>;
    fn read(&mut self, drive: ElementAddress, max_len: usize) -> Result<Vec<u8>>;
}

pub trait TapeInventory {
    fn insert_tape(&mut self, slot: ElementAddress, barcode: TapeBarcode) -> Result<()>;
}
