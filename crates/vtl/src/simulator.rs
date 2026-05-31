use crate::error::{Result, VtlError};
use crate::interface::{MediumChanger, TapeDrive, TapeInventory};
use crate::model::{ElementAddress, ElementKind, TapeBarcode, VirtualTape};

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct Slot {
    address: ElementAddress,
    tape: Option<VirtualTape>,
}

impl Slot {
    fn new(address: ElementAddress) -> Self {
        Self {
            address,
            tape: None,
        }
    }

    pub fn address(&self) -> ElementAddress {
        self.address
    }

    pub fn is_empty(&self) -> bool {
        self.tape.is_none()
    }

    pub fn barcode(&self) -> Option<&TapeBarcode> {
        self.tape.as_ref().map(VirtualTape::barcode)
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct Drive {
    address: ElementAddress,
    tape: Option<VirtualTape>,
}

impl Drive {
    fn new(address: ElementAddress) -> Self {
        Self {
            address,
            tape: None,
        }
    }

    pub fn address(&self) -> ElementAddress {
        self.address
    }

    pub fn is_empty(&self) -> bool {
        self.tape.is_none()
    }

    pub fn loaded_barcode(&self) -> Option<&TapeBarcode> {
        self.tape.as_ref().map(VirtualTape::barcode)
    }

    pub fn loaded_tape(&self) -> Option<&VirtualTape> {
        self.tape.as_ref()
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct VirtualTapeLibrary {
    slots: Vec<Slot>,
    drives: Vec<Drive>,
}

impl VirtualTapeLibrary {
    pub fn new(slot_count: u32, drive_count: u32) -> Self {
        let slots = (1..=slot_count)
            .map(|index| Slot::new(ElementAddress::slot(index)))
            .collect();
        let drives = (0..drive_count)
            .map(|index| Drive::new(ElementAddress::drive(index)))
            .collect();
        Self { slots, drives }
    }

    pub fn slots(&self) -> &[Slot] {
        &self.slots
    }

    pub fn drives(&self) -> &[Drive] {
        &self.drives
    }

    pub fn slot(&self, address: ElementAddress) -> Result<&Slot> {
        self.ensure_slot(address)?;
        self.slots
            .iter()
            .find(|slot| slot.address == address)
            .ok_or_else(|| VtlError::SlotOutOfRange(address.index()))
    }

    pub fn drive(&self, address: ElementAddress) -> Result<&Drive> {
        self.ensure_drive(address)?;
        self.drives
            .iter()
            .find(|drive| drive.address == address)
            .ok_or_else(|| VtlError::DriveOutOfRange(address.index()))
    }

    fn slot_mut(&mut self, address: ElementAddress) -> Result<&mut Slot> {
        self.ensure_slot(address)?;
        self.slots
            .iter_mut()
            .find(|slot| slot.address == address)
            .ok_or_else(|| VtlError::SlotOutOfRange(address.index()))
    }

    fn drive_mut(&mut self, address: ElementAddress) -> Result<&mut Drive> {
        self.ensure_drive(address)?;
        self.drives
            .iter_mut()
            .find(|drive| drive.address == address)
            .ok_or_else(|| VtlError::DriveOutOfRange(address.index()))
    }

    fn ensure_slot(&self, address: ElementAddress) -> Result<()> {
        if address.kind() != ElementKind::Slot {
            return Err(VtlError::WrongElement {
                expected: "slot",
                actual: address.to_string(),
            });
        }
        if address.index() == 0 || address.index() as usize > self.slots.len() {
            return Err(VtlError::SlotOutOfRange(address.index()));
        }
        Ok(())
    }

    fn ensure_drive(&self, address: ElementAddress) -> Result<()> {
        if address.kind() != ElementKind::Drive {
            return Err(VtlError::WrongElement {
                expected: "drive",
                actual: address.to_string(),
            });
        }
        if address.index() as usize >= self.drives.len() {
            return Err(VtlError::DriveOutOfRange(address.index()));
        }
        Ok(())
    }

    fn take_from_slot(&mut self, slot: ElementAddress) -> Result<VirtualTape> {
        self.slot_mut(slot)?
            .tape
            .take()
            .ok_or_else(|| VtlError::SlotEmpty(slot.index()))
    }

    fn put_into_slot(&mut self, slot: ElementAddress, tape: VirtualTape) -> Result<()> {
        let slot_ref = self.slot_mut(slot)?;
        if slot_ref.tape.is_some() {
            return Err(VtlError::SlotOccupied(slot.index()));
        }
        slot_ref.tape = Some(tape);
        Ok(())
    }

    fn take_from_drive(&mut self, drive: ElementAddress) -> Result<VirtualTape> {
        self.drive_mut(drive)?
            .tape
            .take()
            .ok_or_else(|| VtlError::DriveEmpty(drive.index()))
    }

    fn put_into_drive(&mut self, drive: ElementAddress, tape: VirtualTape) -> Result<()> {
        let drive_ref = self.drive_mut(drive)?;
        if drive_ref.tape.is_some() {
            return Err(VtlError::DriveOccupied(drive.index()));
        }
        drive_ref.tape = Some(tape);
        Ok(())
    }

    fn loaded_tape_mut(&mut self, drive: ElementAddress) -> Result<&mut VirtualTape> {
        self.drive_mut(drive)?
            .tape
            .as_mut()
            .ok_or_else(|| VtlError::DriveEmpty(drive.index()))
    }
}

impl TapeInventory for VirtualTapeLibrary {
    fn insert_tape(&mut self, slot: ElementAddress, barcode: TapeBarcode) -> Result<()> {
        self.put_into_slot(slot, VirtualTape::new(barcode))
    }
}

impl MediumChanger for VirtualTapeLibrary {
    fn move_medium(&mut self, from: ElementAddress, to: ElementAddress) -> Result<()> {
        match (from.kind(), to.kind()) {
            (ElementKind::Slot, ElementKind::Slot) => {
                let tape = self.take_from_slot(from)?;
                self.put_into_slot(to, tape)
            }
            (ElementKind::Slot, ElementKind::Drive) => {
                let tape = self.take_from_slot(from)?;
                self.put_into_drive(to, tape)
            }
            (ElementKind::Drive, ElementKind::Slot) => {
                let tape = self.take_from_drive(from)?;
                self.put_into_slot(to, tape)
            }
            (ElementKind::Drive, ElementKind::Drive) => {
                let tape = self.take_from_drive(from)?;
                self.put_into_drive(to, tape)
            }
            _ => Err(VtlError::InvalidElementAddress(format!(
                "unsupported move from {from} to {to}"
            ))),
        }
    }

    fn load(&mut self, slot: ElementAddress, drive: ElementAddress) -> Result<()> {
        self.move_medium(slot, drive)
    }

    fn unload(&mut self, drive: ElementAddress, slot: ElementAddress) -> Result<()> {
        self.move_medium(drive, slot)
    }
}

impl TapeDrive for VirtualTapeLibrary {
    fn rewind(&mut self, drive: ElementAddress) -> Result<()> {
        self.loaded_tape_mut(drive)?.rewind();
        Ok(())
    }

    fn seek_filemark(&mut self, drive: ElementAddress, count: u32) -> Result<()> {
        if self.loaded_tape_mut(drive)?.seek_filemark(count) {
            Ok(())
        } else {
            Err(VtlError::FilemarkNotFound)
        }
    }

    fn write_filemark(&mut self, drive: ElementAddress) -> Result<()> {
        self.loaded_tape_mut(drive)?.append_filemark();
        Ok(())
    }

    fn write(&mut self, drive: ElementAddress, data: &[u8]) -> Result<()> {
        self.loaded_tape_mut(drive)?.append_data(data);
        Ok(())
    }

    fn read(&mut self, drive: ElementAddress, max_len: usize) -> Result<Vec<u8>> {
        Ok(self.loaded_tape_mut(drive)?.read(max_len))
    }
}

impl VirtualTapeLibrary {
    pub fn insert_tape(&mut self, slot: ElementAddress, barcode: TapeBarcode) -> Result<()> {
        <Self as TapeInventory>::insert_tape(self, slot, barcode)
    }

    pub fn move_medium(&mut self, from: ElementAddress, to: ElementAddress) -> Result<()> {
        <Self as MediumChanger>::move_medium(self, from, to)
    }

    pub fn load(&mut self, slot: ElementAddress, drive: ElementAddress) -> Result<()> {
        <Self as MediumChanger>::load(self, slot, drive)
    }

    pub fn unload(&mut self, drive: ElementAddress, slot: ElementAddress) -> Result<()> {
        <Self as MediumChanger>::unload(self, drive, slot)
    }

    pub fn rewind(&mut self, drive: ElementAddress) -> Result<()> {
        <Self as TapeDrive>::rewind(self, drive)
    }

    pub fn seek_filemark(&mut self, drive: ElementAddress, count: u32) -> Result<()> {
        <Self as TapeDrive>::seek_filemark(self, drive, count)
    }

    pub fn write_filemark(&mut self, drive: ElementAddress) -> Result<()> {
        <Self as TapeDrive>::write_filemark(self, drive)
    }

    pub fn write(&mut self, drive: ElementAddress, data: &[u8]) -> Result<()> {
        <Self as TapeDrive>::write(self, drive, data)
    }

    pub fn read(&mut self, drive: ElementAddress, max_len: usize) -> Result<Vec<u8>> {
        <Self as TapeDrive>::read(self, drive, max_len)
    }
}
