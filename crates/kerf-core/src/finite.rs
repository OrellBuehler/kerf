//! A check that a value holds no `NaN` / infinite number.
//!
//! `serde_json` writes a non-finite float as `null`, which a plain `f64` field cannot read
//! back: the timeline blob then fails to load and the project will not open. Every setter
//! that takes a number refuses a non-finite one, but a timeline is also built from the
//! results of arithmetic (ripple, speed, keyframe sampling), so [`Project`] runs this over
//! whatever it is about to store as the last line of defence.
//!
//! [`Project`]: crate::project::Project

use std::fmt;

use serde::ser::{self, Serialize, Serializer};

/// The path (`tracks[0].clips[2].volume`) of the first `NaN` / infinite number inside
/// `value`, or `None` when every number in it is finite. Walks the value through its
/// `Serialize` impl, so it follows exactly what would be written.
pub fn first_non_finite<T: Serialize + ?Sized>(value: &T) -> Option<String> {
    value.serialize(Check).err().map(|found| found.path())
}

#[derive(Debug)]
struct Found {
    /// Innermost segment last; reversed when printed.
    segments: Vec<String>,
    custom: Option<String>,
}

impl Found {
    fn leaf() -> Self {
        Self {
            segments: Vec::new(),
            custom: None,
        }
    }

    fn within(mut self, segment: String) -> Self {
        self.segments.push(segment);
        self
    }

    fn path(&self) -> String {
        let mut out = String::new();
        for segment in self.segments.iter().rev() {
            if segment.starts_with('[') || out.is_empty() {
                out.push_str(segment);
            } else {
                out.push('.');
                out.push_str(segment);
            }
        }
        if out.is_empty() {
            "the value".to_string()
        } else {
            out
        }
    }
}

impl fmt::Display for Found {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.custom {
            Some(why) => f.write_str(why),
            None => write!(f, "non-finite number at {}", self.path()),
        }
    }
}

impl std::error::Error for Found {}

impl ser::Error for Found {
    fn custom<T: fmt::Display>(msg: T) -> Self {
        Self {
            segments: Vec::new(),
            custom: Some(msg.to_string()),
        }
    }
}

struct Check;

/// One struct, map, sequence or tuple being walked.
struct Walk {
    index: usize,
    key: Option<String>,
}

impl Walk {
    fn new() -> Self {
        Self { index: 0, key: None }
    }

    fn element<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<(), Found> {
        let at = self.index;
        self.index += 1;
        value.serialize(Check).map_err(|e| e.within(format!("[{at}]")))
    }

    fn field<T: Serialize + ?Sized>(&mut self, key: &str, value: &T) -> Result<(), Found> {
        value.serialize(Check).map_err(|e| e.within(key.to_string()))
    }
}

macro_rules! passthrough {
    ($($method:ident($ty:ty)),* $(,)?) => {
        $(fn $method(self, _: $ty) -> Result<(), Found> { Ok(()) })*
    };
}

impl Serializer for Check {
    type Ok = ();
    type Error = Found;
    type SerializeSeq = Walk;
    type SerializeTuple = Walk;
    type SerializeTupleStruct = Walk;
    type SerializeTupleVariant = Walk;
    type SerializeMap = Walk;
    type SerializeStruct = Walk;
    type SerializeStructVariant = Walk;

    passthrough!(
        serialize_bool(bool),
        serialize_i8(i8),
        serialize_i16(i16),
        serialize_i32(i32),
        serialize_i64(i64),
        serialize_i128(i128),
        serialize_u8(u8),
        serialize_u16(u16),
        serialize_u32(u32),
        serialize_u64(u64),
        serialize_u128(u128),
        serialize_char(char),
        serialize_str(&str),
        serialize_bytes(&[u8]),
    );

    fn serialize_f32(self, v: f32) -> Result<(), Found> {
        if v.is_finite() {
            Ok(())
        } else {
            Err(Found::leaf())
        }
    }

    fn serialize_f64(self, v: f64) -> Result<(), Found> {
        if v.is_finite() {
            Ok(())
        } else {
            Err(Found::leaf())
        }
    }

    fn serialize_none(self) -> Result<(), Found> {
        Ok(())
    }

    fn serialize_some<T: Serialize + ?Sized>(self, value: &T) -> Result<(), Found> {
        value.serialize(self)
    }

    fn serialize_unit(self) -> Result<(), Found> {
        Ok(())
    }

    fn serialize_unit_struct(self, _: &'static str) -> Result<(), Found> {
        Ok(())
    }

    fn serialize_unit_variant(self, _: &'static str, _: u32, _: &'static str) -> Result<(), Found> {
        Ok(())
    }

    fn serialize_newtype_struct<T: Serialize + ?Sized>(self, _: &'static str, value: &T) -> Result<(), Found> {
        value.serialize(self)
    }

    fn serialize_newtype_variant<T: Serialize + ?Sized>(
        self,
        _: &'static str,
        _: u32,
        variant: &'static str,
        value: &T,
    ) -> Result<(), Found> {
        value.serialize(self).map_err(|e| e.within(variant.to_string()))
    }

    fn serialize_seq(self, _: Option<usize>) -> Result<Walk, Found> {
        Ok(Walk::new())
    }

    fn serialize_tuple(self, _: usize) -> Result<Walk, Found> {
        Ok(Walk::new())
    }

    fn serialize_tuple_struct(self, _: &'static str, _: usize) -> Result<Walk, Found> {
        Ok(Walk::new())
    }

    fn serialize_tuple_variant(self, _: &'static str, _: u32, _: &'static str, _: usize) -> Result<Walk, Found> {
        Ok(Walk::new())
    }

    fn serialize_map(self, _: Option<usize>) -> Result<Walk, Found> {
        Ok(Walk::new())
    }

    fn serialize_struct(self, _: &'static str, _: usize) -> Result<Walk, Found> {
        Ok(Walk::new())
    }

    fn serialize_struct_variant(self, _: &'static str, _: u32, _: &'static str, _: usize) -> Result<Walk, Found> {
        Ok(Walk::new())
    }
}

impl ser::SerializeSeq for Walk {
    type Ok = ();
    type Error = Found;
    fn serialize_element<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<(), Found> {
        self.element(value)
    }
    fn end(self) -> Result<(), Found> {
        Ok(())
    }
}

impl ser::SerializeTuple for Walk {
    type Ok = ();
    type Error = Found;
    fn serialize_element<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<(), Found> {
        self.element(value)
    }
    fn end(self) -> Result<(), Found> {
        Ok(())
    }
}

impl ser::SerializeTupleStruct for Walk {
    type Ok = ();
    type Error = Found;
    fn serialize_field<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<(), Found> {
        self.element(value)
    }
    fn end(self) -> Result<(), Found> {
        Ok(())
    }
}

impl ser::SerializeTupleVariant for Walk {
    type Ok = ();
    type Error = Found;
    fn serialize_field<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<(), Found> {
        self.element(value)
    }
    fn end(self) -> Result<(), Found> {
        Ok(())
    }
}

impl ser::SerializeMap for Walk {
    type Ok = ();
    type Error = Found;
    fn serialize_key<T: Serialize + ?Sized>(&mut self, key: &T) -> Result<(), Found> {
        // Only the name matters for the path; a key that is not a plain string is checked as a value.
        let mut name = String::new();
        key.serialize(KeyName(&mut name))?;
        self.key = Some(name);
        Ok(())
    }
    fn serialize_value<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<(), Found> {
        let key = self.key.take().unwrap_or_default();
        self.field(&key, value)
    }
    fn end(self) -> Result<(), Found> {
        Ok(())
    }
}

impl ser::SerializeStruct for Walk {
    type Ok = ();
    type Error = Found;
    fn serialize_field<T: Serialize + ?Sized>(&mut self, key: &'static str, value: &T) -> Result<(), Found> {
        self.field(key, value)
    }
    fn end(self) -> Result<(), Found> {
        Ok(())
    }
}

impl ser::SerializeStructVariant for Walk {
    type Ok = ();
    type Error = Found;
    fn serialize_field<T: Serialize + ?Sized>(&mut self, key: &'static str, value: &T) -> Result<(), Found> {
        self.field(key, value)
    }
    fn end(self) -> Result<(), Found> {
        Ok(())
    }
}

/// Captures a map key's text for the path; any other shape of key leaves it empty.
struct KeyName<'a>(&'a mut String);

impl Serializer for KeyName<'_> {
    type Ok = ();
    type Error = Found;
    type SerializeSeq = ser::Impossible<(), Found>;
    type SerializeTuple = ser::Impossible<(), Found>;
    type SerializeTupleStruct = ser::Impossible<(), Found>;
    type SerializeTupleVariant = ser::Impossible<(), Found>;
    type SerializeMap = ser::Impossible<(), Found>;
    type SerializeStruct = ser::Impossible<(), Found>;
    type SerializeStructVariant = ser::Impossible<(), Found>;

    fn serialize_str(self, v: &str) -> Result<(), Found> {
        self.0.push_str(v);
        Ok(())
    }

    fn serialize_bool(self, v: bool) -> Result<(), Found> {
        self.0.push_str(&v.to_string());
        Ok(())
    }
    fn serialize_i8(self, v: i8) -> Result<(), Found> {
        self.0.push_str(&v.to_string());
        Ok(())
    }
    fn serialize_i16(self, v: i16) -> Result<(), Found> {
        self.0.push_str(&v.to_string());
        Ok(())
    }
    fn serialize_i32(self, v: i32) -> Result<(), Found> {
        self.0.push_str(&v.to_string());
        Ok(())
    }
    fn serialize_i64(self, v: i64) -> Result<(), Found> {
        self.0.push_str(&v.to_string());
        Ok(())
    }
    fn serialize_u8(self, v: u8) -> Result<(), Found> {
        self.0.push_str(&v.to_string());
        Ok(())
    }
    fn serialize_u16(self, v: u16) -> Result<(), Found> {
        self.0.push_str(&v.to_string());
        Ok(())
    }
    fn serialize_u32(self, v: u32) -> Result<(), Found> {
        self.0.push_str(&v.to_string());
        Ok(())
    }
    fn serialize_u64(self, v: u64) -> Result<(), Found> {
        self.0.push_str(&v.to_string());
        Ok(())
    }
    fn serialize_f32(self, _: f32) -> Result<(), Found> {
        Ok(())
    }
    fn serialize_f64(self, _: f64) -> Result<(), Found> {
        Ok(())
    }
    fn serialize_char(self, v: char) -> Result<(), Found> {
        self.0.push(v);
        Ok(())
    }
    fn serialize_bytes(self, _: &[u8]) -> Result<(), Found> {
        Ok(())
    }
    fn serialize_none(self) -> Result<(), Found> {
        Ok(())
    }
    fn serialize_some<T: Serialize + ?Sized>(self, value: &T) -> Result<(), Found> {
        value.serialize(self)
    }
    fn serialize_unit(self) -> Result<(), Found> {
        Ok(())
    }
    fn serialize_unit_struct(self, _: &'static str) -> Result<(), Found> {
        Ok(())
    }
    fn serialize_unit_variant(self, _: &'static str, _: u32, variant: &'static str) -> Result<(), Found> {
        self.0.push_str(variant);
        Ok(())
    }
    fn serialize_newtype_struct<T: Serialize + ?Sized>(self, _: &'static str, value: &T) -> Result<(), Found> {
        value.serialize(self)
    }
    fn serialize_newtype_variant<T: Serialize + ?Sized>(
        self,
        _: &'static str,
        _: u32,
        _: &'static str,
        _: &T,
    ) -> Result<(), Found> {
        Ok(())
    }
    fn serialize_seq(self, _: Option<usize>) -> Result<Self::SerializeSeq, Found> {
        Err(ser::Error::custom("not a key"))
    }
    fn serialize_tuple(self, _: usize) -> Result<Self::SerializeTuple, Found> {
        Err(ser::Error::custom("not a key"))
    }
    fn serialize_tuple_struct(self, _: &'static str, _: usize) -> Result<Self::SerializeTupleStruct, Found> {
        Err(ser::Error::custom("not a key"))
    }
    fn serialize_tuple_variant(
        self,
        _: &'static str,
        _: u32,
        _: &'static str,
        _: usize,
    ) -> Result<Self::SerializeTupleVariant, Found> {
        Err(ser::Error::custom("not a key"))
    }
    fn serialize_map(self, _: Option<usize>) -> Result<Self::SerializeMap, Found> {
        Err(ser::Error::custom("not a key"))
    }
    fn serialize_struct(self, _: &'static str, _: usize) -> Result<Self::SerializeStruct, Found> {
        Err(ser::Error::custom("not a key"))
    }
    fn serialize_struct_variant(
        self,
        _: &'static str,
        _: u32,
        _: &'static str,
        _: usize,
    ) -> Result<Self::SerializeStructVariant, Found> {
        Err(ser::Error::custom("not a key"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Serialize;
    use std::collections::BTreeMap;

    #[derive(Serialize)]
    struct Inner {
        volume: f32,
        gain: Option<f64>,
    }

    #[derive(Serialize)]
    struct Outer {
        name: String,
        clips: Vec<Inner>,
        pairs: Vec<(f64, f64)>,
        by_name: BTreeMap<String, f64>,
    }

    fn outer() -> Outer {
        Outer {
            name: "a".into(),
            clips: vec![
                Inner { volume: 1.0, gain: None },
                Inner {
                    volume: 0.5,
                    gain: Some(2.0),
                },
            ],
            pairs: vec![(0.0, 1.0)],
            by_name: BTreeMap::from([("x".to_string(), 1.0)]),
        }
    }

    #[test]
    fn a_value_of_finite_numbers_passes() {
        assert_eq!(first_non_finite(&outer()), None);
    }

    #[test]
    fn nan_and_infinity_are_found_with_their_path() {
        let mut v = outer();
        v.clips[1].volume = f32::NAN;
        assert_eq!(first_non_finite(&v).as_deref(), Some("clips[1].volume"));

        let mut v = outer();
        v.clips[0].gain = Some(f64::INFINITY);
        assert_eq!(first_non_finite(&v).as_deref(), Some("clips[0].gain"));

        let mut v = outer();
        v.pairs[0].1 = f64::NEG_INFINITY;
        assert_eq!(first_non_finite(&v).as_deref(), Some("pairs[0][1]"));

        let mut v = outer();
        v.by_name.insert("y".into(), f64::NAN);
        assert_eq!(first_non_finite(&v).as_deref(), Some("by_name.y"));
    }

    #[test]
    fn a_timeline_with_a_poisoned_number_is_found_and_a_clean_one_is_not() {
        let mut timeline = crate::model::Timeline::new();
        assert_eq!(first_non_finite(&timeline), None);
        timeline.master.volume = f64::NAN;
        assert_eq!(first_non_finite(&timeline).as_deref(), Some("master.volume"));
    }
}
