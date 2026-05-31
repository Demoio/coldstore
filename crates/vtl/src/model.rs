use std::fmt;

#[derive(Debug, Clone, Eq, PartialEq, Hash, Ord, PartialOrd)]
pub struct TapeBarcode(String);

impl TapeBarcode {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for TapeBarcode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Debug, Copy, Clone, Eq, PartialEq, Hash)]
pub enum ElementKind {
    Slot,
    Drive,
    ImportExport,
}

impl fmt::Display for ElementKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ElementKind::Slot => f.write_str("slot"),
            ElementKind::Drive => f.write_str("drive"),
            ElementKind::ImportExport => f.write_str("import-export"),
        }
    }
}

#[derive(Debug, Copy, Clone, Eq, PartialEq, Hash)]
pub struct ElementAddress {
    kind: ElementKind,
    index: u32,
}

impl ElementAddress {
    pub fn slot(index: u32) -> Self {
        Self {
            kind: ElementKind::Slot,
            index,
        }
    }

    pub fn drive(index: u32) -> Self {
        Self {
            kind: ElementKind::Drive,
            index,
        }
    }

    pub fn import_export(index: u32) -> Self {
        Self {
            kind: ElementKind::ImportExport,
            index,
        }
    }

    pub fn kind(self) -> ElementKind {
        self.kind
    }

    pub fn index(self) -> u32 {
        self.index
    }

    /// Numeric element value used by `mtx` style commands.
    ///
    /// `mtx load/unload` uses slot number + drive index; `mtx transfer` accepts
    /// numeric element addresses. We keep this value explicit so live wrappers
    /// can be reviewed before they touch a host.
    pub fn mtx_address(self) -> u32 {
        self.index
    }
}

impl fmt::Display for ElementAddress {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.kind, self.index)
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub enum TapeRecord {
    Data(Vec<u8>),
    Filemark,
}

#[derive(Debug, Clone, Eq, PartialEq, Default)]
pub struct TapeCursor {
    pub record_index: usize,
    pub byte_offset: usize,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct VirtualTape {
    barcode: TapeBarcode,
    records: Vec<TapeRecord>,
    cursor: TapeCursor,
}

impl VirtualTape {
    pub fn new(barcode: TapeBarcode) -> Self {
        Self {
            barcode,
            records: Vec::new(),
            cursor: TapeCursor::default(),
        }
    }

    pub fn barcode(&self) -> &TapeBarcode {
        &self.barcode
    }

    pub fn records(&self) -> &[TapeRecord] {
        &self.records
    }

    pub fn cursor(&self) -> &TapeCursor {
        &self.cursor
    }

    pub fn used_bytes(&self) -> u64 {
        self.records
            .iter()
            .map(|record| match record {
                TapeRecord::Data(data) => data.len() as u64,
                TapeRecord::Filemark => 0,
            })
            .sum()
    }

    pub fn current_position(&self) -> u64 {
        let completed_bytes: u64 = self.records[..self.cursor.record_index.min(self.records.len())]
            .iter()
            .map(|record| match record {
                TapeRecord::Data(data) => data.len() as u64,
                TapeRecord::Filemark => 0,
            })
            .sum();
        completed_bytes + self.cursor.byte_offset as u64
    }

    pub fn current_filemark(&self) -> u32 {
        self.records[..self.cursor.record_index.min(self.records.len())]
            .iter()
            .filter(|record| matches!(record, TapeRecord::Filemark))
            .count() as u32
    }

    pub(crate) fn rewind(&mut self) {
        self.cursor = TapeCursor::default();
    }

    pub(crate) fn append_data(&mut self, data: &[u8]) {
        if self.cursor.record_index < self.records.len() {
            self.records.truncate(self.cursor.record_index);
        }
        self.records.push(TapeRecord::Data(data.to_vec()));
        self.cursor.record_index = self.records.len();
        self.cursor.byte_offset = 0;
    }

    pub(crate) fn append_filemark(&mut self) {
        if self.cursor.record_index < self.records.len() {
            self.records.truncate(self.cursor.record_index);
        }
        self.records.push(TapeRecord::Filemark);
        self.cursor.record_index = self.records.len();
        self.cursor.byte_offset = 0;
    }

    pub(crate) fn read(&mut self, max_len: usize) -> Vec<u8> {
        let mut out = Vec::new();
        while out.len() < max_len && self.cursor.record_index < self.records.len() {
            match &self.records[self.cursor.record_index] {
                TapeRecord::Filemark => {
                    self.cursor.record_index += 1;
                    self.cursor.byte_offset = 0;
                    break;
                }
                TapeRecord::Data(data) => {
                    let start = self.cursor.byte_offset.min(data.len());
                    let remaining = max_len - out.len();
                    let end = (start + remaining).min(data.len());
                    out.extend_from_slice(&data[start..end]);
                    if end == data.len() {
                        self.cursor.record_index += 1;
                        self.cursor.byte_offset = 0;
                    } else {
                        self.cursor.byte_offset = end;
                    }
                }
            }
        }
        out
    }

    pub(crate) fn seek_filemark(&mut self, mut count: u32) -> bool {
        while count > 0 && self.cursor.record_index < self.records.len() {
            match &self.records[self.cursor.record_index] {
                TapeRecord::Filemark => {
                    count -= 1;
                    self.cursor.record_index += 1;
                    self.cursor.byte_offset = 0;
                }
                TapeRecord::Data(_) => {
                    self.cursor.record_index += 1;
                    self.cursor.byte_offset = 0;
                }
            }
        }
        count == 0
    }
}
