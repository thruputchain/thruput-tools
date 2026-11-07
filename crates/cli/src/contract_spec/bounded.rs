//! Resource preflight for the generated, recursive `ContractSpec` decoder.
//!
//! Keep the traversal in sync with protocol `contract_spec.x`. It visits only
//! wire structure; named references are leaves, including recursive named types.
//! The generated decoder remains responsible for constructing the actual spec.

use thruput_contract_spec::{
    ContractEnumVariant, ContractMethodEffect, ContractSpecId, ContractSpecString,
    ContractTypeDefKind, ContractTypeKind, ContractUnionCaseKind,
};
use xdrs::{ReadError, ReadXDR, serialize::reader::Reader, txs::buf_reader::BufReader};

// Conservative limits also bound subsequent recursive rendering and drop. The
// work budget counts collection entries and type nodes (including tuple entries
// in both counts), rather than just the largest individual collection.
const MAX_TYPE_DEPTH: usize = 64;
const MAX_WORK: usize = 100_000;
const MAX_BYTES: usize = 16 * 1024 * 1024;

#[derive(Debug, thiserror::Error)]
pub(super) enum Error {
    #[error(transparent)]
    Read(#[from] ReadError),
    #[error("contract spec exceeds type nesting limit ({MAX_TYPE_DEPTH})")]
    Depth,
    #[error("contract spec exceeds work limit ({MAX_WORK})")]
    Work,
    #[error("contract spec exceeds byte limit ({MAX_BYTES})")]
    Bytes,
    #[error("unknown contract spec {0} discriminant: {1}")]
    Discriminant(&'static str, i32),
}

pub(super) fn check(bytes: &[u8]) -> Result<(), Error> {
    if bytes.len() > MAX_BYTES {
        return Err(Error::Bytes);
    }
    let mut scan = Scan {
        reader: BufReader::new(bytes),
        work: MAX_WORK,
    };
    scan.list(Scan::interface)?;
    scan.list(Scan::type_def)?;
    scan.list(Scan::event)?;
    if scan.reader.remaining() != 0 {
        return Err(ReadError::source_remaining(scan.reader.remaining() as u64).into());
    }
    Ok(())
}

struct Scan<'a> {
    reader: BufReader<'a>,
    work: usize,
}

impl<'a> Scan<'a> {
    fn read<T: ReadXDR<'a>>(&mut self) -> Result<T, Error> {
        Ok(T::read_xdr(&mut self.reader)?)
    }

    fn spend(&mut self, count: usize) -> Result<(), Error> {
        self.work = self.work.checked_sub(count).ok_or(Error::Work)?;
        Ok(())
    }

    fn list(&mut self, mut item: impl FnMut(&mut Self) -> Result<(), Error>) -> Result<(), Error> {
        let count = self.reader.read_u32()? as usize;
        // Every list element in this schema consumes at least one XDR word.
        // Check before looping or allowing the generated decoder to allocate.
        if count > self.reader.remaining() / 4 {
            return Err(
                ReadError::source_empty(count.saturating_mul(4), self.reader.remaining()).into(),
            );
        }
        self.spend(count)?;
        for _ in 0..count {
            item(self)?;
        }
        Ok(())
    }

    fn field(&mut self) -> Result<(), Error> {
        self.read::<ContractSpecString>()?;
        self.ty(1)
    }

    fn interface(&mut self) -> Result<(), Error> {
        self.read::<ContractSpecString>()?;
        self.read::<ContractSpecString>()?;
        self.list(|scan| {
            scan.read::<u64>()?;
            scan.ty(1)
        })?;
        self.list(|scan| {
            scan.read::<ContractSpecString>()?;
            scan.list(Self::field)?;
            scan.ty(1)?;
            scan.read::<Option<ContractMethodEffect>>()?;
            Ok(())
        })
    }

    fn type_def(&mut self) -> Result<(), Error> {
        let kind = self.read::<ContractTypeDefKind>()?;
        self.read::<ContractSpecId>()?;
        match kind {
            ContractTypeDefKind::TypeDefStruct => self.list(Self::field),
            ContractTypeDefKind::TypeDefEnum => self.list(|scan| {
                scan.read::<ContractEnumVariant>()?;
                Ok(())
            }),
            ContractTypeDefKind::TypeDefUnion => self.list(|scan| {
                scan.read::<ContractSpecString>()?;
                scan.read::<i32>()?;
                match scan.read::<ContractUnionCaseKind>()? {
                    ContractUnionCaseKind::UnionCaseVoid => Ok(()),
                    ContractUnionCaseKind::UnionCaseTuple => scan.list(|scan| scan.ty(1)),
                    ContractUnionCaseKind::UnionCaseStruct => scan.list(Self::field),
                    kind => Err(Error::Discriminant("union case", kind.0)),
                }
            }),
            kind => Err(Error::Discriminant("type definition", kind.0)),
        }
    }

    fn event(&mut self) -> Result<(), Error> {
        self.read::<ContractSpecId>()?;
        self.reader.read_opaque_fixed(32)?;
        self.list(Self::field)?;
        self.list(Self::field)
    }

    fn ty(&mut self, depth: usize) -> Result<(), Error> {
        if depth > MAX_TYPE_DEPTH {
            return Err(Error::Depth);
        }
        self.spend(1)?;
        match self.read::<ContractTypeKind>()? {
            ContractTypeKind::Void
            | ContractTypeKind::Bool
            | ContractTypeKind::U8
            | ContractTypeKind::U32
            | ContractTypeKind::I32
            | ContractTypeKind::U64
            | ContractTypeKind::I64
            | ContractTypeKind::U128
            | ContractTypeKind::I128
            | ContractTypeKind::U256
            | ContractTypeKind::I256
            | ContractTypeKind::String
            | ContractTypeKind::Bytes
            | ContractTypeKind::Address => Ok(()),
            ContractTypeKind::Option => self.ty(depth + 1),
            ContractTypeKind::Vector | ContractTypeKind::Array => {
                self.ty(depth + 1)?;
                self.read::<u32>()?;
                Ok(())
            }
            ContractTypeKind::Tuple => self.list(|scan| scan.ty(depth + 1)),
            ContractTypeKind::Named => {
                self.read::<ContractSpecId>()?;
                Ok(())
            }
            ContractTypeKind::SignedToken => {
                if self.reader.read_bool()? {
                    self.ty(depth + 1)?;
                }
                Ok(())
            }
            kind => Err(Error::Discriminant("type", kind.0)),
        }
    }
}

#[cfg(test)]
mod tests;
