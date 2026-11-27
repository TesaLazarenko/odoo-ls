use crate::utils::HashMap;

use duplicate::duplicate_item;
use ruff_text_size::TextSize;

use crate::{constants::OYarn, core::symbols::symbol_keys::SymbolKey};

use super::{class_symbol::ClassSymbol, file_symbol::FileSymbol, function_symbol::FunctionSymbol, module_symbol::ModuleSymbol, package_symbol::PythonPackageSymbol};

/// The symbols declared under one name in a body, grouped by section index (see
/// [`SectionRange`]), sorted by section. Replaces a `HashMap<u32, Vec<SymbolKey>>`:
/// a name is nearly always declared once, in one section, and then this holds it inline
/// with no heap allocation (a one-entry hash map with a one-element `Vec` cost ~200 bytes;
/// on Odoo 20.0 that was ~117 MB for ~550k names).
#[derive(Debug, Default, Clone)]
pub struct SectionSymbols(smallvec::SmallVec<[(u32, smallvec::SmallVec<[SymbolKey; 1]>); 1]>);

impl SectionSymbols {
    pub fn get(&self, section: &u32) -> Option<&[SymbolKey]> {
        self.0.binary_search_by_key(section, |(s, _)| *s).ok().map(|i| self.0[i].1.as_slice())
    }
    pub fn values(&self) -> impl Iterator<Item = &[SymbolKey]> {
        self.0.iter().map(|(_, keys)| keys.as_slice())
    }
    pub fn iter(&self) -> impl Iterator<Item = (&u32, &[SymbolKey])> {
        self.0.iter().map(|(s, keys)| (s, keys.as_slice()))
    }
    pub fn push(&mut self, section: u32, key: SymbolKey) {
        match self.0.binary_search_by_key(&section, |(s, _)| *s) {
            Ok(i) => self.0[i].1.push(key),
            Err(i) => self.0.insert(i, (section, smallvec::smallvec![key])),
        }
    }
    /// Heap bytes owned (for memory reports); 0 when everything is inline.
    pub fn heap_bytes(&self) -> usize {
        let outer = if self.0.spilled() { self.0.capacity() * std::mem::size_of::<(u32, smallvec::SmallVec<[SymbolKey; 1]>)>() } else { 0 };
        outer + self.0.iter().filter(|(_, k)| k.spilled()).map(|(_, k)| k.capacity() * std::mem::size_of::<SymbolKey>()).sum::<usize>()
    }
    pub fn len_keys(&self) -> usize { self.0.iter().map(|(_, k)| k.len()).sum() }
    pub fn shrink_to_fit(&mut self) {
        for (_, keys) in self.0.iter_mut() {
            keys.shrink_to_fit();
        }
        self.0.shrink_to_fit();
    }
}

#[derive(Debug, Clone, Default)]
pub struct ContentSymbols {
    pub symbols: Vec<SymbolKey>,
    pub always_defined: bool
}

#[derive(Debug, Clone)]
pub enum SectionIndex {
    INDEX(u32),
    OR(Vec<SectionIndex>),
    NONE,
}

#[derive(Debug, Clone)]
pub struct SectionRange {
    pub start: u32,
    pub index: u32,
    pub previous_indexes: SectionIndex,
}


pub trait SymbolMgr {
    fn get_sections(&self) -> &[SectionRange];
    fn symbols(&self) -> &HashMap<OYarn, SectionSymbols>;
    fn get_section_for(&self, position: u32) -> SectionRange;
    fn get_last_index(&self) -> u32;
    fn add_section(&mut self, range_start: TextSize, maybe_previous_indexes: Option<SectionIndex>) -> SectionRange;
    fn _init_symbol_mgr(&mut self);
    fn reset_sections(&mut self);
}



#[duplicate_item(
    name;
    [FileSymbol];
    [ClassSymbol];
    [FunctionSymbol];
    [ModuleSymbol];
    [PythonPackageSymbol]
)]
impl SymbolMgr for name {
    fn _init_symbol_mgr(&mut self) {
        self.sections.push(SectionRange{
            start: 0,
            index: 0,
            previous_indexes: SectionIndex::NONE
        });
    }

    fn reset_sections(&mut self) {
        self.sections.clear();
        self._init_symbol_mgr();
    }

    fn get_sections(&self) -> &[SectionRange] {
        &self.sections
    }

    fn symbols(&self) -> &HashMap<OYarn, SectionSymbols> {
        &self.symbols
    }

    fn get_section_for(&self, position: u32) -> SectionRange {
        self.sections.iter().rev().find(|section| section.start <= position).unwrap_or(self.sections.last().unwrap()).clone()
    }

    fn get_last_index(&self) -> u32 {
        (self.sections.len() - 1) as u32
    }

    /* Add a section at the END of the sections */
    fn add_section(&mut self, range_start: TextSize, maybe_previous_indexes: Option<SectionIndex>) -> SectionRange{
        let previous_indexes = maybe_previous_indexes.unwrap_or_else(|| {
            let last_index = self.get_last_index();
            SectionIndex::INDEX(last_index)
        });
        let new_section = SectionRange {
            start: range_start.to_u32(),
            index: self.sections.len() as u32,
            previous_indexes,
        };
        self.sections.push(new_section.clone());
        new_section
    }
}

pub fn iter_symbol_keys(symbol: &impl SymbolMgr) -> impl Iterator<Item = & SymbolKey> {
    symbol.symbols().values()
        .flat_map(|section| section.values())
        .flatten()
}
