//! Runtime primitive registry. Unimplemented library
//! declarations produce an explicit Unsupported error if demanded.
use crate::machine::constructors::PRELUDE as ctor;
use crate::machine::{IoAction, PrimitiveArguments, RuntimeErrorKind as Error, ThunkId, Value};
use crate::{numeric, text};
use knot_core::{wrapping_int, Literal, Origin};
use num_bigint::BigInt;
use std::cmp::Ordering;

pub(crate) struct Spec {
    pub arity: usize,
    pub strict: bool,
}
pub(crate) fn spec(name: &str) -> Option<Spec> {
    let (arity, strict) = match name {
        "primIOPure" | "primPutStrLn" => (1, false),
        "primIOBind" => (2, false),
        "primGetLine" => (0, false),
        "error" | "primIntShow" | "primIntToInteger" | "primIntegerShow" | "primIntegerToInt"
        | "primStringLength" => (1, true),
        "primIntEq" | "primIntCompare" | "primIntAdd" | "primIntSub" | "primIntMul"
        | "primIntQuot" | "primIntRem" | "primIntegerEq" | "primIntegerCompare"
        | "primIntegerAdd" | "primIntegerSub" | "primIntegerMul" | "primIntegerQuot"
        | "primIntegerRem" | "primStringEq" | "primStringCompare" | "primStringAppend"
        | "primStringTake" | "primStringDrop" => (2, true),
        "primFloatNegate"
        | "primFloatAbs"
        | "primFloatSignum"
        | "primFloatShow"
        | "primFloatToRational"
        | "primRationalToFloat"
        | "primFloatTruncate"
        | "primIntegerToFloat"
        | "primFloatIsNaN"
        | "primFloatIsInfinite"
        | "primFloatExp"
        | "primFloatLog"
        | "primFloatSqrt"
        | "primFloatSin"
        | "primFloatCos"
        | "primFloatTan"
        | "primFloatAsin"
        | "primFloatAcos"
        | "primFloatAtan"
        | "primFloatSinh"
        | "primFloatCosh"
        | "primFloatTanh"
        | "primFloatAsinh"
        | "primFloatAcosh"
        | "primFloatAtanh" => (1, true),
        "primFloatEq" | "primFloatCompare" | "primFloatLt" | "primFloatLe" | "primFloatGt"
        | "primFloatGe" | "primFloatAdd" | "primFloatSub" | "primFloatMul" | "primFloatDiv"
        | "primFloatPow" | "primFloatAtan2" => (2, true),
        "primStringChr"
        | "primStringConcat"
        | "primStringIsAlpha"
        | "primStringIsAlphaNum"
        | "primStringIsLower"
        | "primStringIsSpace"
        | "primStringIsUpper"
        | "primStringOrd"
        | "primStringShow"
        | "primStringStrip"
        | "primStringToFloat"
        | "primStringToInt"
        | "primStringToList"
        | "primStringToLower"
        | "primStringToUpper" => (1, true),
        "primStringIsInfixOf"
        | "primStringIsPrefixOf"
        | "primStringIsSuffixOf"
        | "primStringSplitOn" => (2, true),
        "primStringReplace" => (3, true),
        _ => return None,
    };
    Some(Spec { arity, strict })
}
fn invalid() -> Error {
    Error::InvalidCore("primitive argument type".into())
}
pub(crate) fn int(v: &Value) -> Result<i64, Error> {
    if let Value::Lit(Literal::Int(n)) = v {
        Ok(*n)
    } else {
        Err(invalid())
    }
}
pub(crate) fn integer(v: &Value) -> Result<&BigInt, Error> {
    if let Value::Lit(Literal::Integer(n)) = v {
        Ok(n)
    } else {
        Err(invalid())
    }
}
pub(crate) fn string(v: &Value) -> Result<&str, Error> {
    if let Value::Lit(Literal::String(s)) = v {
        Ok(s)
    } else {
        Err(invalid())
    }
}
pub(crate) fn float(v: &Value) -> Result<f64, Error> {
    if let Value::Lit(Literal::Float(n)) = v {
        Ok(*n)
    } else {
        Err(invalid())
    }
}
fn bool_value(b: bool) -> Value {
    Value::Ctor(
        if b {
            ctor.true_.clone()
        } else {
            ctor.false_.clone()
        },
        vec![],
    )
}
fn ordering(o: Ordering) -> Value {
    Value::Ctor(
        match o {
            Ordering::Less => &ctor.lt,
            Ordering::Equal => &ctor.eq,
            Ordering::Greater => &ctor.gt,
        }
        .clone(),
        vec![],
    )
}
pub(crate) fn execute(
    name: &str,
    args: &[ThunkId],
    values: &PrimitiveArguments<'_>,
    origin: Origin,
) -> Result<Value, Error> {
    Ok(match name {
        "primIOPure" => Value::Io(IoAction::Pure(args[0]), origin),
        "primIOBind" => Value::Io(IoAction::Bind(args[0], args[1]), origin),
        "primPutStrLn" => Value::Io(IoAction::PutLine(args[0]), origin),
        "primGetLine" => Value::Io(IoAction::GetLine, origin),
        "error" => return Err(Error::Error(string(&values[0])?.into())),
        "primIntEq" => bool_value(int(&values[0])? == int(&values[1])?),
        "primIntCompare" => ordering(int(&values[0])?.cmp(&int(&values[1])?)),
        "primIntShow" => Value::Lit(Literal::String(int(&values[0])?.to_string())),
        "primIntToInteger" => Value::Lit(Literal::Integer(BigInt::from(int(&values[0])?))),
        "primIntegerToInt" => Value::Lit(Literal::Int(wrapping_int(integer(&values[0])?))),
        "primIntAdd" | "primIntSub" | "primIntMul" | "primIntQuot" | "primIntRem" => {
            let a = int(&values[0])?;
            let b = int(&values[1])?;
            let n = match name {
                "primIntAdd" => a.wrapping_add(b),
                "primIntSub" => a.wrapping_sub(b),
                "primIntMul" => a.wrapping_mul(b),
                _ if b == 0 => return Err(Error::DivideByZero),
                "primIntQuot" => a.wrapping_div(b),
                _ => a.wrapping_rem(b),
            };
            Value::Lit(Literal::Int(n))
        }
        "primIntegerEq" => bool_value(integer(&values[0])? == integer(&values[1])?),
        "primIntegerCompare" => ordering(integer(&values[0])?.cmp(integer(&values[1])?)),
        "primIntegerShow" => Value::Lit(Literal::String(integer(&values[0])?.to_string())),
        "primIntegerAdd" | "primIntegerSub" | "primIntegerMul" | "primIntegerQuot"
        | "primIntegerRem" => {
            let a = integer(&values[0])?;
            let b = integer(&values[1])?;
            let n = match name {
                "primIntegerAdd" => a + b,
                "primIntegerSub" => a - b,
                "primIntegerMul" => a * b,
                _ if *b == BigInt::from(0) => return Err(Error::DivideByZero),
                "primIntegerQuot" => a / b,
                _ => a % b,
            };
            Value::Lit(Literal::Integer(n))
        }
        "primIntegerToFloat" => Value::Lit(Literal::Float(numeric::ratio_to_float(
            integer(&values[0])?,
            &1.into(),
        )?)),
        "primFloatShow" => Value::Lit(Literal::String(numeric::show(float(&values[0])?))),
        "primFloatTruncate" => Value::Lit(Literal::Integer(numeric::truncate(float(&values[0])?)?)),
        "primFloatIsNaN" => bool_value(float(&values[0])?.is_nan()),
        "primFloatIsInfinite" => bool_value(float(&values[0])?.is_infinite()),
        "primFloatCompare" => {
            let a = float(&values[0])?;
            let b = float(&values[1])?;
            ordering(if a == b {
                Ordering::Equal
            } else if a < b {
                Ordering::Less
            } else {
                Ordering::Greater
            })
        }
        "primFloatEq" => bool_value(float(&values[0])? == float(&values[1])?),
        "primFloatLt" => bool_value(float(&values[0])? < float(&values[1])?),
        "primFloatLe" => bool_value(float(&values[0])? <= float(&values[1])?),
        "primFloatGt" => bool_value(float(&values[0])? > float(&values[1])?),
        "primFloatGe" => bool_value(float(&values[0])? >= float(&values[1])?),
        "primFloatNegate" => {
            let a = float(&values[0])?;
            Value::Lit(Literal::Float(-a))
        }
        "primFloatAbs" => {
            let a = float(&values[0])?;
            Value::Lit(Literal::Float(a.abs()))
        }
        "primFloatSignum" => {
            let a = float(&values[0])?;
            Value::Lit(Literal::Float(if a == 0.0 || a.is_nan() {
                a
            } else {
                a.signum()
            }))
        }
        "primFloatExp" => {
            let a = float(&values[0])?;
            Value::Lit(Literal::Float(a.exp()))
        }
        "primFloatLog" => {
            let a = float(&values[0])?;
            Value::Lit(Literal::Float(a.ln()))
        }
        "primFloatSqrt" => {
            let a = float(&values[0])?;
            Value::Lit(Literal::Float(a.sqrt()))
        }
        "primFloatSin" => {
            let a = float(&values[0])?;
            Value::Lit(Literal::Float(a.sin()))
        }
        "primFloatCos" => {
            let a = float(&values[0])?;
            Value::Lit(Literal::Float(a.cos()))
        }
        "primFloatTan" => {
            let a = float(&values[0])?;
            Value::Lit(Literal::Float(a.tan()))
        }
        "primFloatAsin" => {
            let a = float(&values[0])?;
            Value::Lit(Literal::Float(a.asin()))
        }
        "primFloatAcos" => {
            let a = float(&values[0])?;
            Value::Lit(Literal::Float(a.acos()))
        }
        "primFloatAtan" => {
            let a = float(&values[0])?;
            Value::Lit(Literal::Float(a.atan()))
        }
        "primFloatSinh" => {
            let a = float(&values[0])?;
            Value::Lit(Literal::Float(a.sinh()))
        }
        "primFloatCosh" => {
            let a = float(&values[0])?;
            Value::Lit(Literal::Float(a.cosh()))
        }
        "primFloatTanh" => {
            let a = float(&values[0])?;
            Value::Lit(Literal::Float(a.tanh()))
        }
        "primFloatAsinh" => {
            let a = float(&values[0])?;
            Value::Lit(Literal::Float(a.asinh()))
        }
        "primFloatAcosh" => {
            let a = float(&values[0])?;
            Value::Lit(Literal::Float(a.acosh()))
        }
        "primFloatAtanh" => {
            let a = float(&values[0])?;
            Value::Lit(Literal::Float(a.atanh()))
        }
        "primFloatAdd" => {
            let a = float(&values[0])?;
            let b = float(&values[1])?;
            Value::Lit(Literal::Float(a + b))
        }
        "primFloatSub" => {
            let a = float(&values[0])?;
            let b = float(&values[1])?;
            Value::Lit(Literal::Float(a - b))
        }
        "primFloatMul" => {
            let a = float(&values[0])?;
            let b = float(&values[1])?;
            Value::Lit(Literal::Float(a * b))
        }
        "primFloatDiv" => {
            let a = float(&values[0])?;
            let b = float(&values[1])?;
            Value::Lit(Literal::Float(a / b))
        }
        "primFloatPow" => {
            let a = float(&values[0])?;
            let b = float(&values[1])?;
            Value::Lit(Literal::Float(a.powf(b)))
        }
        "primFloatAtan2" => {
            let a = float(&values[0])?;
            let b = float(&values[1])?;
            Value::Lit(Literal::Float(a.atan2(b)))
        }
        "primStringShow" => Value::Lit(Literal::String(text::show(string(&values[0])?))),
        "primStringStrip" => Value::Lit(Literal::String(
            string(&values[0])?.trim_matches(text::space).into(),
        )),
        "primStringToUpper" => Value::Lit(Literal::String(
            string(&values[0])?
                .chars()
                .flat_map(char::to_uppercase)
                .collect(),
        )),
        "primStringToLower" => Value::Lit(Literal::String(
            string(&values[0])?
                .chars()
                .flat_map(char::to_lowercase)
                .collect(),
        )),
        "primStringIsPrefixOf" => bool_value(string(&values[1])?.starts_with(string(&values[0])?)),
        "primStringIsSuffixOf" => bool_value(string(&values[1])?.ends_with(string(&values[0])?)),
        "primStringIsInfixOf" => bool_value(string(&values[1])?.contains(string(&values[0])?)),
        "primStringIsSpace"
        | "primStringIsAlpha"
        | "primStringIsAlphaNum"
        | "primStringIsUpper"
        | "primStringIsLower" => bool_value(text::predicate(name, string(&values[0])?)),
        "primStringOrd" => {
            let mut chars = string(&values[0])?.chars();
            let c = chars
                .next()
                .filter(|_| chars.next().is_none())
                .ok_or_else(|| {
                    Error::Error("String.ord: expected one Unicode scalar value".into())
                })?;
            Value::Lit(Literal::Int(c as i64))
        }
        "primStringChr" => {
            let c = u32::try_from(int(&values[0])?)
                .ok()
                .and_then(char::from_u32)
                .ok_or_else(|| Error::Error("String.chr: invalid Unicode scalar value".into()))?;
            Value::Lit(Literal::String(c.to_string()))
        }
        "primStringReplace" => Value::Lit(Literal::String(
            string(&values[2])?.replace(string(&values[0])?, string(&values[1])?),
        )),
        "primStringEq" => bool_value(string(&values[0])? == string(&values[1])?),
        "primStringCompare" => ordering(string(&values[0])?.cmp(string(&values[1])?)),
        "primStringAppend" => Value::Lit(Literal::String(format!(
            "{}{}",
            string(&values[0])?,
            string(&values[1])?
        ))),
        "primStringLength" => Value::Lit(Literal::Int(string(&values[0])?.chars().count() as i64)),
        "primStringTake" | "primStringDrop" => {
            let n = int(&values[0])?.max(0) as usize;
            let s = string(&values[1])?;
            Value::Lit(Literal::String(if name == "primStringTake" {
                s.chars().take(n).collect()
            } else {
                s.chars().skip(n).collect()
            }))
        }
        _ => return Err(Error::Unsupported(format!("primitive {name}"))),
    })
}

/// Size preflight for outputs which can grow beyond their bounded operands.
pub(crate) fn text_output_bytes(
    name: &str,
    values: &PrimitiveArguments<'_>,
) -> Result<Option<usize>, Error> {
    Ok(Some(match name {
        "primStringShow" => text::show_len(string(&values[0])?),
        "primStringToUpper" => string(&values[0])?
            .chars()
            .flat_map(char::to_uppercase)
            .map(char::len_utf8)
            .sum(),
        "primStringToLower" => string(&values[0])?
            .chars()
            .flat_map(char::to_lowercase)
            .map(char::len_utf8)
            .sum(),
        "primStringReplace" => {
            let needle = string(&values[0])?;
            if needle.is_empty() {
                return Err(Error::Error("String.replace: empty pattern".into()));
            }
            let replacement = string(&values[1])?;
            let haystack = string(&values[2])?;
            let count = haystack.matches(needle).count();
            (haystack.len() - count * needle.len())
                .saturating_add(count.saturating_mul(replacement.len()))
        }
        _ => return Ok(None),
    }))
}

/// Deliberate gaps in current runtime coverage. The inventory test requires every
/// library primitive to be implemented or listed here.
#[cfg(test)]
const DEFERRED: &[&str] = &[];

#[cfg(test)]
mod tests {
    use super::*;
    use knot_canonical::ast::{CDecl, CType};
    #[test]
    fn registry_matches_library_names_and_implemented_arities() {
        let implemented = [
            "primStringChr",
            "primStringConcat",
            "primStringIsAlpha",
            "primStringIsAlphaNum",
            "primStringIsLower",
            "primStringIsSpace",
            "primStringIsUpper",
            "primStringOrd",
            "primStringShow",
            "primStringStrip",
            "primStringToFloat",
            "primStringToInt",
            "primStringToList",
            "primStringToLower",
            "primStringToUpper",
            "primStringIsInfixOf",
            "primStringIsPrefixOf",
            "primStringIsSuffixOf",
            "primStringSplitOn",
            "primStringReplace",
            "primFloatNegate",
            "primFloatAbs",
            "primFloatSignum",
            "primFloatShow",
            "primFloatToRational",
            "primRationalToFloat",
            "primFloatTruncate",
            "primIntegerToFloat",
            "primFloatIsNaN",
            "primFloatIsInfinite",
            "primFloatExp",
            "primFloatLog",
            "primFloatSqrt",
            "primFloatSin",
            "primFloatCos",
            "primFloatTan",
            "primFloatAsin",
            "primFloatAcos",
            "primFloatAtan",
            "primFloatSinh",
            "primFloatCosh",
            "primFloatTanh",
            "primFloatAsinh",
            "primFloatAcosh",
            "primFloatAtanh",
            "primFloatEq",
            "primFloatCompare",
            "primFloatLt",
            "primFloatLe",
            "primFloatGt",
            "primFloatGe",
            "primFloatAdd",
            "primFloatSub",
            "primFloatMul",
            "primFloatDiv",
            "primFloatPow",
            "primFloatAtan2",
            "error",
            "primGetLine",
            "primIOBind",
            "primIOPure",
            "primIntAdd",
            "primIntCompare",
            "primIntEq",
            "primIntMul",
            "primIntQuot",
            "primIntRem",
            "primIntShow",
            "primIntSub",
            "primIntToInteger",
            "primIntegerAdd",
            "primIntegerCompare",
            "primIntegerEq",
            "primIntegerMul",
            "primIntegerQuot",
            "primIntegerRem",
            "primIntegerShow",
            "primIntegerSub",
            "primIntegerToInt",
            "primPutStrLn",
            "primStringAppend",
            "primStringCompare",
            "primStringDrop",
            "primStringEq",
            "primStringLength",
            "primStringTake",
        ];
        let mut declared = std::collections::BTreeSet::new();
        for (name, _) in knot_canonical::prelude::LIBRARY {
            let module = knot_canonical::canonicalize_library(name).unwrap();
            for declaration in module.decls {
                if let CDecl::Primitive(name, sig) = declaration.node {
                    if name == ":" {
                        continue;
                    } // lowered as a constructor
                    assert!(declared.insert(name.clone()));
                    let mut ty = &sig.node.ty;
                    let mut arity = 0;
                    while let CType::Fn(_, result) = ty {
                        arity += 1;
                        ty = result;
                    }
                    match spec(&name) {
                        Some(s) => {
                            assert_eq!(s.arity, arity, "{name}");
                            assert!(!DEFERRED.contains(&name.as_str()));
                        }
                        None => assert!(
                            DEFERRED.contains(&name.as_str()),
                            "unclassified primitive {name}"
                        ),
                    }
                }
            }
        }
        let recorded: std::collections::BTreeSet<_> = implemented
            .iter()
            .chain(DEFERRED)
            .map(|s| s.to_string())
            .collect();
        assert_eq!(recorded, declared);
    }
}
