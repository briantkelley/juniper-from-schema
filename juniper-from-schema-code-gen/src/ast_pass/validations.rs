use super::schema_visitor::SchemaVisitor;
use super::EmitError;
use super::ErrorKind;
use graphql_parser::schema::{self, *};
use graphql_parser::Pos;
use heck::SnakeCase;

pub struct FieldNameCaseValidator<'pass, T> {
    pass: &'pass mut T,
}

impl<'pass, 'doc, T> FieldNameCaseValidator<'pass, T>
where
    T: EmitError<'doc>,
{
    pub fn new(pass: &'pass mut T) -> Self {
        Self { pass }
    }
}

impl<'pass, 'doc, T> SchemaVisitor<'doc> for FieldNameCaseValidator<'pass, T>
where
    T: EmitError<'doc>,
{
    fn visit_object_type(&mut self, ty: &'doc schema::ObjectType<&'doc str>) {
        self.validate_fields(&ty.fields);
    }

    fn visit_interface_type(&mut self, ty: &'doc schema::InterfaceType<&'doc str>) {
        self.validate_fields(&ty.fields);
    }

    fn visit_input_object_type(&mut self, ty: &'doc schema::InputObjectType<&'doc str>) {
        for field in &ty.fields {
            self.validate_field(&field.name, field.position);
        }
    }
}

impl<'pass, 'doc, T> FieldNameCaseValidator<'pass, T>
where
    T: EmitError<'doc>,
{
    fn validate_fields(&mut self, fields: &'doc [Field<&'doc str>]) {
        for field in fields {
            self.validate_field(&field.name, field.position);
        }
    }

    fn validate_field(&mut self, name: &str, pos: Pos) {
        if is_snake_case(name) {
            self.pass.emit_error(pos, ErrorKind::FieldNameInSnakeCase);
        }
    }
}

pub struct UuidNameCaseValidator<'pass, T> {
    pass: &'pass mut T,
}

impl<'pass, 'doc, T> UuidNameCaseValidator<'pass, T>
where
    T: EmitError<'doc>,
{
    pub fn new(pass: &'pass mut T) -> Self {
        Self { pass }
    }
}

impl<'pass, 'doc, T> SchemaVisitor<'doc> for UuidNameCaseValidator<'pass, T>
where
    T: EmitError<'doc>,
{
    fn visit_scalar_type(&mut self, scalar: &'doc ScalarType<&'doc str>) {
        if scalar.name == "UUID" {
            self.pass
                .emit_error(scalar.position, ErrorKind::UppercaseUuidScalar);
        }
    }
}

fn is_snake_case(s: &str) -> bool {
    s.contains('_') && s.to_snake_case() == s
}

#[cfg(test)]
mod test {
    #[allow(unused_imports)]
    use super::*;

    #[test]
    fn test_is_snake_case() {
        assert!(is_snake_case("foo_bar"));
        assert!(is_snake_case("foo_bar_baz"));

        assert!(!is_snake_case("foo"));
        assert!(!is_snake_case("fooBar"));
        assert!(!is_snake_case("FooBar"));
    }
}