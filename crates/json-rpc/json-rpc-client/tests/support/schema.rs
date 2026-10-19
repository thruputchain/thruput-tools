//! Offline schema validation with exact decimal range extensions.
use serde_json::{Value, json};

struct IntegerBound {
    value: i128,
    minimum: bool,
}
impl<'i> jsonschema::Keyword<'i> for IntegerBound {
    fn validate(&self, instance: &'i Value) -> Result<(), jsonschema::ValidationError<'i>> {
        if self.is_valid(instance) {
            Ok(())
        } else {
            Err(jsonschema::ValidationError::custom(
                "decimal integer outside declared range",
            ))
        }
    }
    fn is_valid(&self, instance: &'i Value) -> bool {
        instance
            .as_str()
            .and_then(|v| v.parse::<i128>().ok())
            .is_some_and(|v| {
                if self.minimum {
                    v >= self.value
                } else {
                    v <= self.value
                }
            })
    }
}

pub fn at(document: &Value, pointer: &str) -> jsonschema::Validator {
    let mut schema = document.clone();
    schema["$ref"] = json!(format!("#{pointer}"));
    jsonschema::options()
        .with_keyword("x-integer-minimum", |_, bound: &Value, _| {
            Ok(Box::new(IntegerBound {
                value: bound.as_str().unwrap().parse().unwrap(),
                minimum: true,
            }))
        })
        .with_keyword("x-integer-maximum", |_, bound: &Value, _| {
            Ok(Box::new(IntegerBound {
                value: bound.as_str().unwrap().parse().unwrap(),
                minimum: false,
            }))
        })
        .build(&schema)
        .unwrap()
}
