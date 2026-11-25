//! Direct typed-value bridge. Preserve source float width without text round trips.
use serde_json::{Value, json};
use unity_rs_core::type_tree::TypeValue;
fn number(value: f64) -> Value {
    serde_json::Number::from_f64(value).map_or_else(
        || {
            json!(if value.is_nan() {
                "NaN"
            } else if value.is_sign_negative() {
                "-Infinity"
            } else {
                "Infinity"
            })
        },
        Value::Number,
    )
}
fn convert(value: TypeValue, remaining: &mut usize, depth: usize) -> Result<Value, String> {
    if depth > 128 || *remaining < 32 {
        return Err("Native value conversion exceeds its budget".into());
    }
    *remaining -= 32;
    Ok(match value {
        TypeValue::Signed(v)=>json!(v),TypeValue::Unsigned(v)=>json!(v),TypeValue::Boolean(v)=>json!(v),
        TypeValue::Float32(v)=>number(f64::from(v)),TypeValue::Float(v)=>number(v),
        TypeValue::Character(v)=>json!(char::from_u32(u32::from(v)).unwrap_or(char::REPLACEMENT_CHARACTER).to_string()),
        TypeValue::String(v)=>{*remaining=remaining.checked_sub(v.len()).ok_or("Native string exceeds conversion budget")?;Value::String(v)},
        TypeValue::TypelessData{offset,size}=>json!({"Offset":offset,"Size":size}),
        TypeValue::Array(values)=>Value::Array(values.into_iter().map(|v|convert(v,remaining,depth+1)).collect::<Result<_,_>>()?),
        TypeValue::ByteArray{element,bytes}=>{
            if bytes.len()>*remaining/32{return Err("Native byte array exceeds conversion budget".into());}
            Value::Array(bytes.into_iter().map(|v|convert(element.value(v),remaining,depth+1)).collect::<Result<_,_>>()?)
        },
        TypeValue::Object(fields)=>{
            let mut result=serde_json::Map::new();
            for field in fields{*remaining=remaining.checked_sub(field.name.len()).ok_or("Native field names exceed conversion budget")?;result.insert(field.name,convert(field.value,remaining,depth+1)?);}
            Value::Object(result)
        },
        TypeValue::Map(entries)=>Value::Array(entries.into_iter().map(|entry|Ok(json!({"key":convert(entry.key,remaining,depth+1)?,"value":convert(entry.value,remaining,depth+1)?}))).collect::<Result<_,String>>()?),
        _=>return Err("Unsupported native type-tree value variant".into()),
    })
}
pub fn into_value(value: TypeValue) -> Result<Value, String> {
    convert(value, &mut (64 * 1024 * 1024), 0)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn source_float_bits_and_large_integer_ids_survive_without_text_rounding() {
        let source = 0.3f32;
        assert_eq!(
            into_value(TypeValue::Float32(source))
                .unwrap()
                .as_f64()
                .unwrap()
                .to_bits(),
            f64::from(source).to_bits()
        );
        let double = 0.3826834323650898f64;
        assert_eq!(
            into_value(TypeValue::Float(double))
                .unwrap()
                .as_f64()
                .unwrap()
                .to_bits(),
            double.to_bits()
        );
        assert_eq!(
            into_value(TypeValue::Signed(i64::MIN)).unwrap().as_i64(),
            Some(i64::MIN)
        );
        assert!(convert(TypeValue::String("too long".into()), &mut 33, 0).is_err());
    }
}
