mod gen_query_trails;

use super::{
    directive_parsing::*, error::Error, schema_visitor::*, type_name, validations::*, AstData,
    DateTimeScalarDefinition, EmitError, ErrorKind, NullableType, TypeKind,
};
use graphql_parser::{schema, schema::Value, Pos};
use heck::{CamelCase, SnakeCase};
use proc_macro2::{Span, TokenStream};
use quote::{format_ident, quote, ToTokens};
use std::{
    collections::{BTreeMap, BTreeSet, HashSet},
    convert::TryFrom,
};
use syn::{parse_quote, Ident, LitStr, Token};

#[derive(Debug)]
pub struct CodeGenPass<'doc> {
    error_type: &'doc syn::Type,
    context_type: &'doc syn::Type,
    errors: BTreeSet<Error>,
    ast_data: AstData<'doc>,
    scalars: Vec<Scalar<'doc>>,
    objects: Vec<Object<'doc>>,
    subscription: Option<Subscription<'doc>>,
    interfaces: Vec<Interface<'doc>>,
    unions: Vec<Union<'doc>>,
    enums: Vec<Enum<'doc>>,
    input_objects: Vec<InputObject<'doc>>,
    schema_type: Option<SchemaType>,
}

impl<'doc> CodeGenPass<'doc> {
    pub fn new(
        error_type: &'doc syn::Type,
        context_type: &'doc syn::Type,
        ast_data: AstData<'doc>,
    ) -> Self {
        Self {
            error_type,
            context_type,
            ast_data,
            errors: BTreeSet::new(),
            scalars: Vec::new(),
            objects: Vec::new(),
            subscription: None,
            interfaces: Vec::new(),
            unions: Vec::new(),
            enums: Vec::new(),
            input_objects: Vec::new(),
            schema_type: None,
        }
    }

    pub fn gen_juniper_code(
        mut self,
        doc: &'doc schema::Document<'doc, &'doc str>,
    ) -> Result<TokenStream, BTreeSet<Error>> {
        self.validate_doc(doc);
        self.check_for_errors()?;

        let query_trail_tokens = self.gen_query_trails(doc);
        visit_document(&mut self, doc);

        self.check_for_errors()?;

        let Self {
            scalars,
            objects,
            subscription,
            interfaces,
            unions,
            enums,
            input_objects,
            schema_type,

            error_type: _,
            context_type: _,
            errors: _,
            ast_data: _,
        } = self;

        let mut tokens = quote! {
            #(#scalars)*
            #(#objects)*
            #subscription
            #(#interfaces)*
            #(#unions)*
            #(#enums)*
            #(#input_objects)*
            #schema_type
        };

        // eprintln!("\n");
        // eprintln!("{}", tokens);
        // eprintln!("\n");

        tokens.extend(query_trail_tokens);

        Ok(tokens)
    }

    fn validate_doc(&mut self, doc: &'doc schema::Document<'doc, &'doc str>) {
        let mut validation_visitor =
            FieldNameCaseValidator::new().and(UuidNameCaseValidator::new());

        visit_document(&mut validation_visitor, doc);

        let (field_validator, uuid_name_validator) = validation_visitor.into_inner();
        for error in field_validator
            .errors
            .into_iter()
            .chain(uuid_name_validator.errors)
        {
            self.errors.insert(error);
        }
    }

    fn check_for_errors(&self) -> Result<(), BTreeSet<Error>> {
        if self.errors.is_empty() {
            Ok(())
        } else {
            Err(self.errors.clone())
        }
    }
}

impl<'doc> EmitError for CodeGenPass<'doc> {
    fn emit_error(&mut self, pos: Pos, kind: ErrorKind) {
        self.errors.emit_error(pos, kind)
    }
}

impl<'doc> SchemaVisitor<'doc> for CodeGenPass<'doc> {
    fn visit_schema_definition(&mut self, node: &'doc schema::SchemaDefinition<'doc, &'doc str>) {
        let schema::SchemaDefinition {
            position,
            directives: _,
            query,
            mutation,
            subscription,
        } = node;

        let query_type = match query {
            Some(query) => {
                let ident = format_ident!("{}", query);
                parse_quote! { #ident }
            }
            None => {
                self.emit_error(*position, ErrorKind::NoQueryType);
                return;
            }
        };

        let mutation_type = match mutation {
            Some(mutation) => {
                let ident = format_ident!("{}", mutation);
                parse_quote! { #ident }
            }
            None => {
                let context_type = &self.context_type;
                parse_quote! { juniper_from_schema::juniper::EmptyMutation<#context_type> }
            }
        };

        let subscription_type = match subscription {
            Some(subscription) => {
                let ident = format_ident!("{}", subscription);
                parse_quote! { #ident }
            }
            None => {
                let context_type = &self.context_type;
                parse_quote! { juniper_from_schema::juniper::EmptySubscription<#context_type> }
            }
        };

        self.schema_type = Some(SchemaType {
            query_type,
            mutation_type,
            subscription_type,
        });
    }

    fn visit_directive_definition(
        &mut self,
        node: &'doc schema::DirectiveDefinition<'doc, &'doc str>,
    ) {
        if node.name == "juniper" {
            self.validate_juniper_directive_definition(node)
        }
    }

    fn visit_scalar_type(&mut self, node: &'doc schema::ScalarType<'doc, &'doc str>) {
        match &*node.name {
            name if name == crate::DATE_TIME_SCALAR_NAME => {
                // This case is special because it supports a directive. We don't need to parse and
                // check the it though that is done by `AstData::visit_scalar_type`

                if node.description.is_some() {
                    self.emit_error(node.position, ErrorKind::SpecialCaseScalarWithDescription);
                }
            }
            name if name == crate::DATE_SCALAR_NAME
                || name == crate::URL_SCALAR_NAME
                || name == crate::UUID_SCALAR_NAME =>
            {
                let () = self.parse_directives(node);

                if node.description.is_some() {
                    self.emit_error(node.position, ErrorKind::SpecialCaseScalarWithDescription);
                }
            }
            _ => {
                let schema::ScalarType {
                    position,
                    description,
                    name,
                    directives: _,
                } = node;

                let () = self.parse_directives(node);

                match &**name {
                    "String" | "Float" | "Int" | "Boolean" | "ID" => {
                        self.emit_error(*position, ErrorKind::CannotDeclareBuiltinAsScalar);
                    }
                    _ => {}
                }

                self.scalars.push(Scalar {
                    name: format_ident!("{}", name),
                    description: description.as_ref(),
                });
            }
        };
    }

    fn visit_object_type(&mut self, node: &'doc schema::ObjectType<'doc, &'doc str>) {
        let schema::ObjectType {
            position,
            description,
            name,
            implements_interfaces,
            directives: _,
            fields,
        } = node;

        let () = self.parse_directives(node);

        if self.ast_data.is_subscription_type(name) {
            if !implements_interfaces.is_empty() {
                self.emit_error(*position, ErrorKind::SubscriptionsCannotImplementInterfaces);
            }

            let fields = fields
                .iter()
                .map(|field| self.graphql_field_to_rust_field(field, FieldLocation::Subscription))
                .collect();

            self.subscription = Some(Subscription {
                name: format_ident!("{}", name),
                description: description.as_ref(),
                context_type: self.context_type,
                fields,
            });
        } else {
            let fields = fields
                .iter()
                .map(|field| self.graphql_field_to_rust_field(field, FieldLocation::Object))
                .collect();

            let implements_interfaces = implements_interfaces
                .iter()
                .map(|name| format_ident!("{}", name))
                .collect();

            self.objects.push(Object {
                name: format_ident!("{}", name),
                description: description.as_ref(),
                context_type: self.context_type,
                fields,
                implements_interfaces,
            });
        }
    }

    fn visit_interface_type(&mut self, node: &'doc schema::InterfaceType<'doc, &'doc str>) {
        let schema::InterfaceType {
            description,
            name,
            fields,
            position: _,
            directives: _,
        } = node;

        let () = self.parse_directives(node);

        let implementors = self
            .ast_data
            .get_implementors_of_interface(name)
            .cloned()
            .unwrap_or_else(Vec::new)
            .into_iter()
            .map(|name| format_ident!("{}", name))
            .collect::<Vec<_>>();

        let name = format_ident!("{}", name);
        let fields = fields
            .iter()
            .map(|field| self.graphql_field_to_rust_field(field, FieldLocation::Interface))
            .collect();

        self.interfaces.push(Interface {
            description: description.as_ref(),
            trait_name: format_ident!("{}Interface", name),
            name,
            fields,
            implementors,
            context_type: self.context_type,
        });
    }

    fn visit_union_type(&mut self, node: &'doc schema::UnionType<'doc, &'doc str>) {
        let schema::UnionType {
            position,
            description,
            name,
            types,
            directives: _,
        } = node;

        let () = self.parse_directives(node);

        let name = format_ident!("{}", name);

        let variants = types
            .iter()
            .map(|variant_name| {
                let graphql_type: schema::Type<'doc, &'doc str> =
                    schema::Type::NamedType(*variant_name);
                let type_inside = self
                    .graphql_type_to_rust_type(&graphql_type, false, *position)
                    .remove_one_layer_of_nullability_by_value();
                let ident = format_ident!("{}", variant_name);
                UnionVariant {
                    rust_name: ident,
                    type_inside,
                }
            })
            .collect::<Vec<_>>();

        self.unions.push(Union {
            name,
            variants,
            description: description.as_ref(),
            context_type: self.context_type,
        })
    }

    fn visit_enum_type(&mut self, node: &'doc schema::EnumType<'doc, &'doc str>) {
        let schema::EnumType {
            description,
            name,
            values,
            position: _,
            directives: _,
        } = node;

        let () = self.parse_directives(node);

        let name = format_ident!("{}", name);

        let variants = values
            .iter()
            .map(|value| {
                let schema::EnumValue {
                    name,
                    description,
                    position: _,
                    directives: _,
                } = value;

                let graphql_name = *name;
                let name = format_ident!("{}", name.to_camel_case());
                let deprecation = self.parse_directives(value);

                EnumVariant {
                    name,
                    deprecation,
                    description: description.as_ref(),
                    graphql_name,
                }
            })
            .collect();

        self.enums.push(Enum {
            name,
            variants,
            description: description.as_ref(),
        })
    }

    fn visit_input_object_type(&mut self, node: &'doc schema::InputObjectType<'doc, &'doc str>) {
        let schema::InputObjectType {
            description,
            name,
            fields,

            position: _,
            directives: _,
        } = node;

        let () = self.parse_directives(node);

        let name = format_ident!("{}", name);
        let fields = fields
            .iter()
            .map(|field| {
                let schema::InputValue {
                    description,
                    name,
                    value_type,
                    default_value,
                    position,
                    directives: _,
                } = field;

                let () = self.parse_directives(field);

                if default_value.is_some() {
                    self.emit_error(*position, ErrorKind::InputTypeFieldWithDefaultValue);
                }

                let ty = self.graphql_type_to_rust_type(value_type, false, *position);

                let name = format_ident!("{}", name.to_snake_case());

                InputObjectField {
                    name,
                    ty,
                    description: description.as_ref(),
                }
            })
            .collect::<Vec<_>>();

        self.input_objects.push(InputObject {
            name,
            description: description.as_ref(),
            fields,
        });
    }

    fn visit_scalar_type_extension(
        &mut self,
        inner: &'doc schema::ScalarTypeExtension<'doc, &'doc str>,
    ) {
        self.emit_error(inner.position, ErrorKind::TypeExtensionNotSupported)
    }

    fn visit_object_type_extension(
        &mut self,
        inner: &'doc schema::ObjectTypeExtension<'doc, &'doc str>,
    ) {
        self.emit_error(inner.position, ErrorKind::TypeExtensionNotSupported)
    }

    fn visit_interface_type_extension(
        &mut self,
        inner: &'doc schema::InterfaceTypeExtension<'doc, &'doc str>,
    ) {
        self.emit_error(inner.position, ErrorKind::TypeExtensionNotSupported)
    }

    fn visit_union_type_extension(
        &mut self,
        inner: &'doc schema::UnionTypeExtension<'doc, &'doc str>,
    ) {
        self.emit_error(inner.position, ErrorKind::TypeExtensionNotSupported)
    }

    fn visit_enum_type_extension(
        &mut self,
        inner: &'doc schema::EnumTypeExtension<'doc, &'doc str>,
    ) {
        self.emit_error(inner.position, ErrorKind::TypeExtensionNotSupported)
    }

    fn visit_input_object_type_extension(
        &mut self,
        inner: &'doc schema::InputObjectTypeExtension<'doc, &'doc str>,
    ) {
        self.emit_error(inner.position, ErrorKind::TypeExtensionNotSupported)
    }
}

impl<'doc> CodeGenPass<'doc> {
    fn graphql_field_to_rust_field(
        &mut self,
        field: &'doc schema::Field<'doc, &'doc str>,
        field_location: FieldLocation,
    ) -> Field<'doc> {
        let schema::Field {
            position,
            description,
            name,
            arguments,
            field_type,
            directives: _,
        } = field;

        let field_directives = self.parse_directives(field);

        self.validate_directive_for_field(&field_directives, field_location, *position);

        let args = arguments
            .iter()
            .map(|arg| {
                let schema::InputValue {
                    position,
                    description,
                    name,
                    value_type,
                    default_value,
                    directives: _,
                } = arg;

                let () = self.parse_directives(arg);

                let default_value = default_value
                    .as_ref()
                    .map(|v| self.quote_value(v, type_name(value_type), *position));

                let ty = self.graphql_type_to_rust_type(value_type, false, *position);

                if default_value.is_some() && !ty.is_nullable() {
                    self.emit_error(*position, ErrorKind::NonnullableFieldWithDefaultValue);
                }

                let name_without_raw_ident = format_ident!("{}", name.to_snake_case());
                FieldArg {
                    name: format_ident!("r#{}", name_without_raw_ident),
                    name_without_raw_ident,
                    description: description.as_ref(),
                    ty,
                    default_value,
                }
            })
            .collect();

        let return_type = self.graphql_type_to_rust_type(
            field_type,
            field_directives.ownership.is_as_ref(),
            *position,
        );

        if field_directives.ownership == Ownership::AsRef && !return_type.supports_as_ref() {
            self.emit_error(*position, ErrorKind::AsRefOwnershipForNamedType);
        }

        Field {
            description: description.as_ref(),
            name: format_ident!("r#{}", name.to_snake_case()),
            context_type: self.context_type,
            error_type: self.error_type,
            args,
            return_type,
            directives: field_directives,
        }
    }

    fn graphql_type_to_rust_type(
        &mut self,
        graphql_type: &schema::Type<'doc, &'doc str>,
        as_ref: bool,
        pos: Pos,
    ) -> Type {
        fn gen_leaf<'doc>(pass: &CodeGenPass<'doc>, name: &'doc str) -> Type {
            match &*name {
                "String" => Type::Scalar(Either::A(parse_quote! { std::string::String })),
                "Float" => Type::Scalar(Either::A(parse_quote! { f64 })),
                "Int" => Type::Scalar(Either::A(parse_quote! { i32 })),
                "Boolean" => Type::Scalar(Either::A(parse_quote! { bool })),
                "ID" => Type::Scalar(Either::A(parse_quote! { juniper_from_schema::juniper::ID })),
                name => {
                    if pass.ast_data.is_scalar(name) {
                        Type::Scalar(Either::B(format_ident!("{}", name)))
                    } else if pass.ast_data.is_enum_type(name) {
                        Type::Enum(format_ident!("{}", name))
                    } else if pass.ast_data.is_union_type(name) {
                        Type::Union(format_ident!("{}", name))
                    } else if pass.ast_data.is_interface_type(name) {
                        Type::Interface(format_ident!("{}", name))
                    } else {
                        Type::Object(format_ident!("{}", name))
                    }
                }
            }
        }

        fn gen_node<'doc>(
            pass: &mut CodeGenPass<'doc>,
            ty: &NullableType<'doc>,
            as_ref: bool,
            pos: Pos,
        ) -> Type {
            match ty {
                NullableType::NamedType(inner) => match &**inner {
                    name if name == crate::URL_SCALAR_NAME => {
                        if !pass.ast_data.url_scalar_defined() {
                            pass.emit_error(pos, ErrorKind::UrlScalarNotDefined);
                        }
                        Type::Scalar(Either::A(parse_quote! { url::Url }))
                    }

                    name if name == crate::UUID_SCALAR_NAME => {
                        if !pass.ast_data.uuid_scalar_defined() {
                            pass.emit_error(pos, ErrorKind::UuidScalarNotDefined);
                        }
                        Type::Scalar(Either::A(parse_quote! { uuid::Uuid }))
                    }

                    name if name == crate::DATE_SCALAR_NAME => {
                        if !pass.ast_data.date_scalar_defined() {
                            pass.emit_error(pos, ErrorKind::DateScalarNotDefined);
                        }
                        Type::Scalar(Either::A(parse_quote! { chrono::naive::NaiveDate }))
                    }

                    name if name == crate::DATE_TIME_SCALAR_NAME => {
                        match pass.ast_data.date_time_scalar_definition() {
                            Some(DateTimeScalarDefinition::WithTimeZone) => Type::Scalar(
                                Either::A(parse_quote! { chrono::DateTime<chrono::offset::Utc> }),
                            ),

                            Some(DateTimeScalarDefinition::WithoutTimeZone) => Type::Scalar(
                                Either::A(parse_quote! { chrono::naive::NaiveDateTime }),
                            ),

                            None => {
                                pass.emit_error(pos, ErrorKind::DateTimeScalarNotDefined);

                                Type::Scalar(Either::A(
                                    parse_quote! { chrono::DateTime<chrono::offset::Utc> },
                                ))
                            }
                        }
                    }

                    _ => gen_leaf(pass, inner),
                },
                NullableType::ListType(inner) => {
                    if as_ref {
                        Type::List(Box::new(Type::Ref(Box::new(gen_node(
                            pass, &*inner, false, pos,
                        )))))
                    } else {
                        Type::List(Box::new(gen_node(pass, &*inner, false, pos)))
                    }
                }
                NullableType::NullableType(inner) => {
                    if as_ref {
                        Type::Nullable(Box::new(Type::Ref(Box::new(gen_node(
                            pass, &*inner, false, pos,
                        )))))
                    } else {
                        Type::Nullable(Box::new(gen_node(pass, &*inner, false, pos)))
                    }
                }
            }
        }

        let nullable_type = NullableType::from_schema_type(graphql_type);
        gen_node(self, &nullable_type, as_ref, pos)
    }

    fn quote_value(
        &mut self,
        value: &'doc Value<'doc, &'doc str>,
        type_name: &'doc str,
        pos: Pos,
    ) -> TokenStream {
        match value {
            Value::Float(inner) => quote! { #inner },
            Value::Int(inner) => {
                let number = inner
                    .as_i64()
                    .expect("failed to convert default number argument to i64");
                let number = i32::try_from(number)
                    .expect("failed to convert default number argument to i64");
                quote! { #number }
            }
            Value::String(inner) => quote! { #inner.to_string() },
            Value::Boolean(inner) => quote! { #inner },

            Value::Enum(variant_name) => {
                let type_name = format_ident!("{}", type_name.to_camel_case());
                let variant_name = format_ident!("{}", variant_name.to_camel_case());
                quote! { #type_name::#variant_name }
            }

            Value::List(list) => {
                let mut acc = quote! { let mut vec = Vec::new(); };
                for value in list {
                    let value_quoted = self.quote_value(value, type_name, pos);
                    acc.extend(quote! { vec.push(#value_quoted); });
                }
                acc.extend(quote! { vec });
                quote! { { #acc } }
            }

            Value::Object(map) => self.quote_object_value(map, type_name, pos),

            Value::Variable(_) => {
                self.emit_error(pos, ErrorKind::VariableDefaultValue);
                quote! {}
            }

            Value::Null => quote! { None },
        }
    }

    fn quote_object_value(
        &mut self,
        map: &'doc BTreeMap<&'doc str, Value<'doc, &'doc str>>,
        type_name: &'doc str,
        pos: Pos,
    ) -> TokenStream {
        let name = format_ident!("{}", type_name);

        let mut fields_seen: HashSet<&'doc str> = HashSet::new();

        // Set fields given in `map`
        let mut field_assigments = map
            .iter()
            .map(|(key, value)| {
                fields_seen.insert(key);
                let field_name = format_ident!("{}", key.to_snake_case());

                let field_type_name = self
                    .ast_data
                    .input_object_field_type_name(&type_name, &key)
                    .unwrap_or_else(|| {
                        panic!("input_object_field_type_name {} {}", type_name, key)
                    });

                let value_quote = self.quote_value(value, field_type_name, pos);
                match self
                    .ast_data
                    .input_object_field_is_nullable(&type_name, &key)
                {
                    Some(true) | None => {
                        if value == &Value::Null {
                            quote! { #field_name: #value_quote }
                        } else {
                            quote! { #field_name: Some(#value_quote) }
                        }
                    }
                    Some(false) => quote! { #field_name: #value_quote },
                }
            })
            .collect::<Vec<_>>();

        // Set fields not given in map to `None`
        if let Some(fields) = self.ast_data.input_object_field_names(&type_name) {
            for field_name in fields {
                if !fields_seen.contains(field_name) {
                    let field_name = format_ident!("{}", field_name.to_snake_case());
                    field_assigments.push(quote! {
                        #field_name: None
                    });
                }
            }
        }

        let tokens = quote! {
            #name {
                #(#field_assigments),*,
            }
        };

        quote! { { #tokens } }
    }

    fn validate_juniper_directive_definition(
        &mut self,
        directive: &'doc schema::DirectiveDefinition<'doc, &'doc str>,
    ) {
        use schema::{DirectiveLocation, InputValue, Type as GraphqlType};

        assert_eq!(directive.name, "juniper");

        let mut field_location_present = false;
        let mut scalar_location_present = false;

        for location in directive.locations.iter() {
            match location {
                DirectiveLocation::FieldDefinition => {
                    field_location_present = true;
                }
                DirectiveLocation::Scalar => {
                    scalar_location_present = true;
                }
                other => self.emit_error(
                    directive.position,
                    ErrorKind::InvalidJuniperDirective(
                        format!(
                            "Invalid location for @juniper directive: `{}`",
                            other.as_str()
                        ),
                        Some("Location must be `FIELD_DEFINITION | SCALAR`".to_string()),
                    ),
                ),
            }
        }

        if !field_location_present {
            self.emit_error(
                directive.position,
                ErrorKind::InvalidJuniperDirective(
                    "Missing `FIELD_DEFINITION` directive location for @juniper directive"
                        .to_string(),
                    Some("Location must be `FIELD_DEFINITION | SCALAR`".to_string()),
                ),
            )
        }

        if !scalar_location_present {
            self.emit_error(
                directive.position,
                ErrorKind::InvalidJuniperDirective(
                    "Missing `SCALAR` directive location for @juniper directive".to_string(),
                    Some("Location must be `FIELD_DEFINITION | SCALAR`".to_string()),
                ),
            )
        }

        let no_directives = |this: &mut Self, arg: &InputValue<'doc, &'doc str>, name: &str| {
            for dir in arg.directives.iter() {
                this.emit_error(
                    dir.position,
                    ErrorKind::InvalidJuniperDirective(
                        format!("`{}` argument doesn't support directives", name),
                        None,
                    ),
                )
            }
        };

        let of_type = |this: &mut Self,
                       arg: &InputValue<'doc, &'doc str>,
                       ty: GraphqlType<'doc, &'doc str>,
                       name: &str| {
            if arg.value_type != ty {
                this.emit_error(
                    arg.position,
                    ErrorKind::InvalidJuniperDirective(
                        format!("`{}` argument must have type `{}`", name, ty),
                        Some(format!("Got `{}`", arg.value_type)),
                    ),
                )
            }
        };

        let default_value = |this: &mut Self,
                             arg: &InputValue<'doc, &'doc str>,
                             value: Value<'doc, &'doc str>,
                             name: &str| {
            if let Some(default) = &arg.default_value {
                if default == &value {
                    // ok
                } else {
                    this.emit_error(
                        arg.position,
                        ErrorKind::InvalidJuniperDirective(
                            format!(
                                "Invalid default value for `{}` argument. Must be `{}`",
                                name, value
                            ),
                            Some(format!("Got `{}`", default)),
                        ),
                    )
                }
            } else {
                this.emit_error(
                    arg.position,
                    ErrorKind::InvalidJuniperDirective(
                        format!(
                            "Missing default value for `{}` argument. Must be `{}`",
                            name, value
                        ),
                        None,
                    ),
                )
            }
        };

        let mut ownership_present = false;
        let mut infallible_present = false;
        let mut with_time_zone_present = false;
        let mut async_present = false;
        let mut stream_item_infallible_present = false;
        let mut stream_type_present = false;

        for arg in directive.arguments.iter() {
            match arg.name {
                name @ "ownership" => {
                    ownership_present = true;
                    of_type(self, arg, GraphqlType::NamedType("String"), name);
                    no_directives(self, arg, name);
                    default_value(self, arg, Value::String("borrowed".to_string()), name);
                }
                name @ "infallible" => {
                    infallible_present = true;
                    of_type(self, arg, GraphqlType::NamedType("Boolean"), name);
                    no_directives(self, arg, name);
                    default_value(self, arg, Value::Boolean(false), name);
                }
                name @ "with_time_zone" => {
                    with_time_zone_present = true;
                    of_type(self, arg, GraphqlType::NamedType("Boolean"), name);
                    no_directives(self, arg, name);
                    default_value(self, arg, Value::Boolean(true), name);
                }
                name @ "async" => {
                    async_present = true;
                    of_type(self, arg, GraphqlType::NamedType("Boolean"), name);
                    no_directives(self, arg, name);
                    default_value(self, arg, Value::Boolean(false), name);
                }
                name @ "stream_item_infallible" => {
                    stream_item_infallible_present = true;
                    of_type(self, arg, GraphqlType::NamedType("Boolean"), name);
                    no_directives(self, arg, name);
                    default_value(self, arg, Value::Boolean(true), name);
                }
                name @ "stream_type" => {
                    stream_type_present = true;
                    of_type(self, arg, GraphqlType::NamedType("String"), name);
                    no_directives(self, arg, name);
                    default_value(self, arg, Value::Null, name);
                }
                name => {
                    self.emit_error(
                        arg.position,
                        ErrorKind::InvalidJuniperDirective(
                            format!("Invalid argument for @juniper directive: `{}`", name),
                            Some(
                                "Supported arguments are `ownership`, `infallible`, `with_time_zone`, `async`, `stream_item_infallible`, and `stream_type`".to_string()
                            ),
                        ),
                    )
                }
            }
        }

        if !ownership_present {
            self.emit_error(
                directive.position,
                ErrorKind::InvalidJuniperDirective(
                    "Missing argument `ownership`".to_string(),
                    None,
                ),
            )
        }

        if !infallible_present {
            self.emit_error(
                directive.position,
                ErrorKind::InvalidJuniperDirective(
                    "Missing argument `infallible`".to_string(),
                    None,
                ),
            )
        }

        if !with_time_zone_present {
            self.emit_error(
                directive.position,
                ErrorKind::InvalidJuniperDirective(
                    "Missing argument `with_time_zone`".to_string(),
                    None,
                ),
            )
        }

        if !async_present {
            self.emit_error(
                directive.position,
                ErrorKind::InvalidJuniperDirective("Missing argument `async`".to_string(), None),
            )
        }

        if !stream_item_infallible_present {
            self.emit_error(
                directive.position,
                ErrorKind::InvalidJuniperDirective(
                    "Missing argument `stream_item_infallible`".to_string(),
                    None,
                ),
            )
        }

        if !stream_type_present {
            self.emit_error(
                directive.position,
                ErrorKind::InvalidJuniperDirective(
                    "Missing argument `stream_type`".to_string(),
                    None,
                ),
            )
        }
    }

    fn validate_directive_for_field(
        &mut self,
        directives: &FieldDirectives,
        field_location: FieldLocation,
        pos: Pos,
    ) {
        match field_location {
            FieldLocation::Object | FieldLocation::Interface => {
                if directives.stream_type.is_some() {
                    self.emit_error(pos, ErrorKind::StreamTypeNotSupportedHere);
                }

                if directives.stream_item_infallible.is_some() {
                    self.emit_error(pos, ErrorKind::StreamItemInfallibleNotSupportedHere);
                }
            }
            FieldLocation::Subscription => {
                match &directives.ownership {
                    Ownership::Borrowed | Ownership::AsRef => {
                        self.emit_error(pos, ErrorKind::SubscriptionFieldMustBeOwned);
                    }
                    Ownership::Owned => {}
                }

                if let Some(ty) = &directives.stream_type {
                    if let Err(err) = syn::parse_str::<syn::Type>(&ty.value) {
                        self.emit_error(pos, ErrorKind::InvalidStreamReturnType(err.to_string()));
                    }
                }
            }
        }
    }
}

#[derive(Debug, Clone)]
enum Either<A, B> {
    A(A),
    B(B),
}

impl<A, B> ToTokens for Either<A, B>
where
    A: ToTokens,
    B: ToTokens,
{
    fn to_tokens(&self, tokens: &mut TokenStream) {
        match self {
            Either::A(a) => a.to_tokens(tokens),
            Either::B(b) => b.to_tokens(tokens),
        }
    }
}

#[derive(Debug, Clone)]
enum Type {
    Scalar(Either<syn::Type, Ident>),
    Enum(Ident),
    Union(Ident),
    Interface(Ident),
    Object(Ident),
    Ref(Box<Type>),
    List(Box<Type>),
    Nullable(Box<Type>),
}

impl Type {
    fn is_nullable(&self) -> bool {
        matches!(self, Type::Nullable(_))
    }

    fn supports_as_ref(&self) -> bool {
        match self {
            Type::Scalar(_) => false,
            Type::Enum(_) => false,
            Type::Union(_) => false,
            Type::Interface(_) => false,
            Type::Object(_) => false,
            Type::Ref(_) => false,
            Type::List(_) => true,
            Type::Nullable(_) => true,
        }
    }

    fn remove_one_layer_of_nullability_by_value(self) -> Box<Type> {
        match self {
            Type::Nullable(inner) => inner,
            other => Box::new(other),
        }
    }

    fn remove_one_layer_of_nullability(&self) -> &Type {
        match self {
            Type::Nullable(inner) => inner,
            other => other,
        }
    }

    fn kind(&self) -> TypeKind {
        match self {
            Type::Scalar(_) => TypeKind::Scalar,
            Type::Enum(_) => TypeKind::Scalar,
            Type::Union(_) => TypeKind::Type,
            Type::Object(_) => TypeKind::Type,
            Type::Interface { .. } => TypeKind::Type,
            Type::Ref(inner) => inner.kind(),
            Type::List(inner) => inner.kind(),
            Type::Nullable(inner) => inner.kind(),
        }
    }

    fn innermost_type(&self) -> &Type {
        match self {
            Type::Scalar(_) => self,
            Type::Enum(_) => self,
            Type::Union(_) => self,
            Type::Object(_) => self,
            Type::Interface { .. } => self,
            Type::Ref(inner) => inner.innermost_type(),
            Type::List(inner) => inner.innermost_type(),
            Type::Nullable(inner) => inner.innermost_type(),
        }
    }
}

impl ToTokens for Type {
    fn to_tokens(&self, tokens: &mut TokenStream) {
        let code = match self {
            Type::Scalar(inner) => {
                quote! { #inner }
            }
            Type::Enum(inner) => {
                quote! { #inner }
            }
            Type::Union(inner) => {
                quote! { #inner }
            }
            Type::Object(inner) => {
                quote! { #inner }
            }
            Type::Interface(inner) => {
                quote! { #inner }
            }
            Type::Ref(inner) => {
                quote! { &#inner }
            }
            Type::List(inner) => {
                quote! { std::vec::Vec<#inner> }
            }
            Type::Nullable(inner) => {
                quote! { std::option::Option<#inner> }
            }
        };
        tokens.extend(code);
    }
}

#[derive(Debug, Default)]
struct Output {}

#[derive(Debug)]
struct Scalar<'doc> {
    name: Ident,
    description: Option<&'doc String>,
}

impl<'doc> ToTokens for Scalar<'doc> {
    fn to_tokens(&self, tokens: &mut TokenStream) {
        let Scalar { name, description } = self;

        let attrs = if let Some(description) = description {
            quote! {
                #[derive(juniper_from_schema::juniper::GraphQLScalarValue)]
                #[graphql(
                    transparent,
                    description = #description,
                )]
            }
        } else {
            quote! {
                #[derive(juniper_from_schema::juniper::GraphQLScalarValue)]
                #[graphql(transparent)]
            }
        };

        let code = quote! {
            #[derive(Debug, Eq, PartialEq, Ord, PartialOrd, Clone, Hash)]
            #attrs
            pub struct #name(pub std::string::String);

            impl #name {
                pub fn new<S>(s: S) -> Self
                where
                    Self: std::convert::From<S>,
                {
                    #name::from(s)
                }
            }

            impl std::convert::From<std::string::String> for #name {
                fn from(s: std::string::String) -> #name {
                    #name(s)
                }
            }

            impl std::convert::From<&str> for #name {
                fn from(s: &str) -> #name {
                    #name(s.to_string())
                }
            }

            impl<'a, 'b> query_trails::FromLookAheadValue<#name>
                for &'a juniper_from_schema::juniper::LookAheadValue<'b, juniper_from_schema::juniper::DefaultScalarValue>
            {
                fn from(self) -> #name {
                    let s = query_trails::FromLookAheadValue::<String>::from(self);
                    #name(s)
                }
            }
        };

        tokens.extend(code);
    }
}

#[derive(Debug)]
struct Object<'doc> {
    name: Ident,
    description: Option<&'doc String>,
    context_type: &'doc syn::Type,
    fields: Vec<Field<'doc>>,
    implements_interfaces: Vec<Ident>,
}

impl<'doc> ToTokens for Object<'doc> {
    fn to_tokens(&self, tokens: &mut TokenStream) {
        let Object {
            name,
            context_type,
            description,
            fields,
            implements_interfaces,
        } = self;

        let mut graphql_attrs = GraphqlAttr::new_object();

        if let Some(description) = description {
            graphql_attrs.push_key_value(format_ident!("description"), description);
        }

        graphql_attrs.push_key_value(format_ident!("Context"), context_type);
        graphql_attrs.push_key_value(
            format_ident!("Scalar"),
            quote! { juniper_from_schema::juniper::DefaultScalarValue },
        );

        if !implements_interfaces.is_empty() {
            graphql_attrs.push_key_value(
                format_ident!("impl"),
                quote! { #(#implements_interfaces),* },
            );
        }

        let trait_name = fields_trait_name(name);

        let fields_for_impl = fields
            .iter()
            .map(|field| field.to_tokens_for_graphql_object_impl(&trait_name));

        let fields_for_trait = fields.iter().map(|field| field.to_tokens_for_trait());

        let async_trait_attr = if fields.iter().any(|f| f.directives.r#async.value) {
            Some(quote! { #[juniper_from_schema::juniper::async_trait] })
        } else {
            None
        };

        let code = quote! {
            #graphql_attrs
            impl #name {
                #(#fields_for_impl)*
            }

            #async_trait_attr
            pub trait #trait_name {
                #(#fields_for_trait)*
            }
        };

        tokens.extend(code);
    }
}

fn fields_trait_name(name: &Ident) -> Ident {
    format_ident!("{}Fields", name)
}

#[derive(Debug)]
struct Field<'doc> {
    description: Option<&'doc String>,
    name: Ident,
    error_type: &'doc syn::Type,
    context_type: &'doc syn::Type,
    args: Vec<FieldArg<'doc>>,
    return_type: Type,
    directives: FieldDirectives,
}

impl<'doc> Field<'doc> {
    fn to_tokens_for_graphql_object_impl<'a>(
        &'a self,
        trait_name: &'a Ident,
    ) -> FieldToTokensGraphqlObject<'a, 'doc> {
        FieldToTokensGraphqlObject {
            field: self,
            trait_name,
        }
    }

    fn to_tokens_for_trait<'a>(&'a self) -> FieldToTokensTrait<'a, 'doc> {
        FieldToTokensTrait { field: self }
    }

    fn to_tokens_for_interface<'a>(&'a self) -> FieldToTokensInterface<'a, 'doc> {
        FieldToTokensInterface { field: self }
    }

    fn to_tokens_for_interface_impl<'a>(
        &'a self,
        trait_name: &'a Ident,
    ) -> FieldToTokensInterfaceImpl<'a, 'doc> {
        FieldToTokensInterfaceImpl {
            field: self,
            trait_name,
        }
    }

    fn to_tokens_for_subscription_impl<'a>(
        &'a self,
        trait_name: &'a Ident,
    ) -> FieldToTokensForSubscriptionImpl<'a, 'doc> {
        FieldToTokensForSubscriptionImpl {
            field: self,
            trait_name,
        }
    }

    fn to_tokens_for_subscription_trait<'a>(
        &'a self,
    ) -> FieldToTokensForSubscriptionTrait<'a, 'doc> {
        FieldToTokensForSubscriptionTrait { field: self }
    }

    fn trait_field_name(&self) -> Ident {
        format_ident!("field_{}", self.name)
    }

    fn asyncness(&self) -> Option<Token![async]> {
        if self.directives.r#async.value {
            Some(syn::token::Async::default())
        } else {
            None
        }
    }

    fn awaitness(&self) -> Option<TokenStream> {
        if self.directives.r#async.value {
            Some(quote! { .await })
        } else {
            None
        }
    }

    fn return_type_not_wrapped_in_result(&self) -> syn::Type {
        let return_type = &self.return_type;

        match &self.directives.ownership {
            Ownership::Owned => {
                parse_quote! { #return_type }
            }
            Ownership::Borrowed => {
                parse_quote! { &#return_type }
            }
            Ownership::AsRef => {
                // this case is handled in `graphql_type_to_rust_type`
                parse_quote! { #return_type }
            }
        }
    }

    fn full_return_type(&self) -> syn::Type {
        maybe_wrap_final_return_type_in_result(
            self.return_type_not_wrapped_in_result(),
            &self.error_type,
            &self.directives,
        )
    }

    fn full_stream_return_type(&self) -> syn::Type {
        let default_return_type = || {
            let item_type = self.stream_item_type();
            parse_quote! {
                std::pin::Pin<
                    std::boxed::Box<
                        dyn juniper_from_schema::futures::Stream<Item = #item_type>
                            + std::marker::Send
                    >
                >
            }
        };

        if let Some(ty) = &self.directives.stream_type {
            let ty = syn::parse_str(&ty.value).unwrap_or_else(|_| default_return_type());
            maybe_wrap_final_return_type_in_result(ty, &self.error_type, &self.directives)
        } else {
            maybe_wrap_final_return_type_in_result(
                default_return_type(),
                &self.error_type,
                &self.directives,
            )
        }
    }

    fn stream_item_type(&self) -> syn::Type {
        if let Some(stream_item_infallible) = &self.directives.stream_item_infallible {
            if stream_item_infallible.value {
                self.return_type_not_wrapped_in_result()
            } else {
                let ty = self.return_type_not_wrapped_in_result();
                let error_type = &self.error_type;
                parse_quote! { std::result::Result<#ty, #error_type> }
            }
        } else {
            self.return_type_not_wrapped_in_result()
        }
    }

    fn query_trail_type(&self) -> &Type {
        self.return_type.innermost_type()
    }

    fn query_trail_param(&self) -> Option<TokenStream> {
        match self.return_type.kind() {
            TypeKind::Type => {
                let query_trail_type = self.query_trail_type();
                Some(quote! {
                    trail: &juniper_from_schema::QueryTrail<'r, #query_trail_type, juniper_from_schema::Walked>,
                })
            }
            TypeKind::Scalar => None,
        }
    }
}

#[derive(Debug)]
struct FieldToTokensGraphqlObject<'a, 'doc> {
    field: &'a Field<'doc>,
    trait_name: &'a Ident,
}

#[allow(unused_variables, warnings)]
impl<'a, 'doc> ToTokens for FieldToTokensGraphqlObject<'a, 'doc> {
    fn to_tokens(&self, tokens: &mut TokenStream) {
        let Field {
            description,
            name,
            error_type: _,
            context_type: _,
            args,
            return_type: _,
            directives,
        } = self.field;

        let mut graphql_attrs = GraphqlAttr::new();

        if !args.is_empty() {
            let parts = args.iter().filter_map(|arg| {
                let name = &arg.name_without_raw_ident;

                if let Some(description) = &arg.description {
                    Some(quote! {
                        #name(description = #description)
                    })
                } else {
                    None
                }
            });

            graphql_attrs.push_fn(format_ident!("arguments"), parts);
        };

        add_deprecation_graphql_attr_token(directives, &mut graphql_attrs);

        if let Some(description) = description {
            graphql_attrs.push_key_value(format_ident!("description"), description);
        };

        let trait_name = self.trait_name;
        let trait_field_name = self.field.trait_field_name();
        let arg_names = args.iter().map(|arg| &arg.name);
        let return_type = self.field.full_return_type();

        let args_for_signature = args
            .iter()
            .map(|arg| arg.to_tokens_for_graphql_object_impl());

        let rebind_args_with_default_values = args.iter().filter_map(|arg| {
            if let Some(default_value) = &arg.default_value {
                let name = &arg.name;
                Some(quote! { let #name = #name.unwrap_or_else(|| #default_value); })
            } else {
                None
            }
        });

        let query_trail_arg = if self.field.query_trail_param().is_some() {
            let query_trail_type = self.field.query_trail_type();
            quote! {
                &juniper_from_schema::QueryTrail::<
                    #query_trail_type,
                    juniper_from_schema::Walked,
                >::new(&executor.look_ahead()),
            }
        } else {
            quote! {}
        };

        let asyncness = self.field.asyncness();
        let awaitness = self.field.awaitness();

        tokens.extend(quote! {
            #graphql_attrs
            #asyncness fn #name(
                &self,
                executor: &Executor,
                #(#args_for_signature,)*
            ) -> #return_type {
                #(#rebind_args_with_default_values)*
                <Self as #trait_name>::#trait_field_name(
                    self,
                    executor,
                    #query_trail_arg
                    #(#arg_names,)*
                ) #awaitness
            }
        });
    }
}

#[derive(Debug)]
struct FieldToTokensTrait<'a, 'doc> {
    field: &'a Field<'doc>,
}

impl<'a, 'doc> ToTokens for FieldToTokensTrait<'a, 'doc> {
    fn to_tokens(&self, tokens: &mut TokenStream) {
        let Field {
            description: _,
            name: _,
            error_type: _,
            context_type,
            args,
            return_type: _,
            directives: _,
        } = self.field;

        let name = self.field.trait_field_name();
        let full_return_type = self.field.full_return_type();

        let args = args.iter().map(|arg| arg.to_tokens_for_trait());

        let query_trail_param = self.field.query_trail_param();

        let asyncness = self.field.asyncness();

        tokens.extend(quote! {
            #asyncness fn #name<'s, 'r, 'a>(
                &'s self,
                executor: &juniper_from_schema::juniper::Executor<'r, 'a, #context_type>,
                #query_trail_param
                #(#args,)*
            ) -> #full_return_type;
        });
    }
}

#[derive(Debug)]
struct FieldToTokensInterface<'a, 'doc> {
    field: &'a Field<'doc>,
}

impl<'a, 'doc> ToTokens for FieldToTokensInterface<'a, 'doc> {
    fn to_tokens(&self, tokens: &mut TokenStream) {
        let Field {
            description,
            name,
            error_type: _,
            context_type,
            args,
            return_type: _,
            directives,
        } = self.field;

        let return_type = self.field.full_return_type();

        let args = args.iter().map(|arg| arg.to_tokens_for_interface());

        let mut graphql_attrs = GraphqlAttr::new();

        if let Some(desc) = description {
            graphql_attrs.push_key_value(format_ident!("description"), desc);
        }

        add_deprecation_graphql_attr_token(directives, &mut graphql_attrs);

        let asyncness = self.field.asyncness();

        tokens.extend(quote! {
            #graphql_attrs
            #asyncness fn #name<'s, 'r, 'a>(
                &'s self,
                executor: &juniper_from_schema::juniper::Executor<
                    'a,
                    'r,
                    #context_type,
                    juniper_from_schema::juniper::DefaultScalarValue,
                >,
                #(#args,)*
            ) -> #return_type;
        })
    }
}

#[derive(Debug)]
struct FieldToTokensInterfaceImpl<'a, 'doc> {
    field: &'a Field<'doc>,
    trait_name: &'a Ident,
}

impl<'a, 'doc> ToTokens for FieldToTokensInterfaceImpl<'a, 'doc> {
    fn to_tokens(&self, tokens: &mut TokenStream) {
        let FieldToTokensInterfaceImpl {
            field:
                Field {
                    description: _,
                    name,
                    error_type: _,
                    context_type,
                    args,
                    return_type: _,
                    directives: _,
                },
            trait_name,
        } = self;

        // TODO: Remove duplication between this and the object version

        let trait_field_name = self.field.trait_field_name();
        let arg_names = args.iter().map(|arg| &arg.name);
        let full_return_type = self.field.full_return_type();

        // juniper doesn't supporte descriptions on interface field arguments so we cannot add
        // those

        let args_for_signature = args
            .iter()
            .map(|arg| arg.to_tokens_for_graphql_object_impl());

        let rebind_args_with_default_values = args.iter().filter_map(|arg| {
            if let Some(default_value) = &arg.default_value {
                let name = &arg.name;
                Some(quote! { let #name = #name.unwrap_or_else(|| #default_value); })
            } else {
                None
            }
        });

        let query_trail_arg = if self.field.query_trail_param().is_some() {
            let query_trail_type = self.field.query_trail_type();
            quote! {
                &juniper_from_schema::QueryTrail::<
                    #query_trail_type,
                    juniper_from_schema::Walked,
                >::new(&executor.look_ahead()),
            }
        } else {
            quote! {}
        };

        let asyncness = self.field.asyncness();
        let awaitness = self.field.awaitness();

        let code = quote! {
            #asyncness fn #name<'s, 'r, 'a>(
                &'s self,
                executor: &juniper_from_schema::juniper::Executor<
                    'a,
                    'r,
                    #context_type,
                    juniper_from_schema::juniper::DefaultScalarValue,
                >,
                #(#args_for_signature),*
            ) -> #full_return_type {
                #(#rebind_args_with_default_values)*
                <Self as #trait_name>::#trait_field_name(
                    self,
                    executor,
                    #query_trail_arg
                    #(#arg_names,)*
                ) #awaitness
            }
        };
        tokens.extend(code)
    }
}

#[derive(Debug)]
struct FieldToTokensForSubscriptionImpl<'a, 'doc> {
    field: &'a Field<'doc>,
    trait_name: &'a Ident,
}

impl<'a, 'doc> ToTokens for FieldToTokensForSubscriptionImpl<'a, 'doc> {
    fn to_tokens(&self, tokens: &mut TokenStream) {
        let Field {
            description,
            name,
            args,
            error_type: _,
            context_type: _,
            return_type: _,
            directives: _,
        } = self.field;

        let mut graphql_attrs = GraphqlAttr::new();

        if let Some(description) = description {
            graphql_attrs.push_key_value(format_ident!("description"), description);
        };

        if !args.is_empty() {
            let parts = args.iter().filter_map(|arg| {
                let name = &arg.name_without_raw_ident;

                if let Some(description) = &arg.description {
                    Some(quote! {
                        #name(description = #description)
                    })
                } else {
                    None
                }
            });

            graphql_attrs.push_fn(format_ident!("arguments"), parts);
        };

        let trait_name = self.trait_name;
        let trait_field_name = self.field.trait_field_name();
        let arg_names = args.iter().map(|arg| &arg.name);

        let return_type = self.field.full_stream_return_type();

        let args_for_signature = args
            .iter()
            .map(|arg| arg.to_tokens_for_graphql_object_impl());

        let rebind_args_with_default_values = args.iter().filter_map(|arg| {
            if let Some(default_value) = &arg.default_value {
                let name = &arg.name;
                Some(quote! { let #name = #name.unwrap_or_else(|| #default_value); })
            } else {
                None
            }
        });

        let query_trail_arg = if self.field.query_trail_param().is_some() {
            let query_trail_type = self.field.query_trail_type();
            quote! {
                &juniper_from_schema::QueryTrail::<
                    #query_trail_type,
                    juniper_from_schema::Walked,
                >::new(&executor.look_ahead()),
            }
        } else {
            quote! {}
        };

        let awaitness = self.field.awaitness();

        let tryness = if self.field.directives.infallible.value {
            None
        } else {
            Some(quote! { ? })
        };

        let mut return_result = if self.field.directives.stream_type.is_some() {
            quote! { resolved_value }
        } else {
            quote! { std::boxed::Box::pin(resolved_value) }
        };

        if !self.field.directives.infallible.value {
            return_result = quote! { Ok(#return_result) };
        }

        let code = quote! {
            #graphql_attrs
            async fn #name(
                executor: &Executor,
                #(#args_for_signature,)*
            ) -> #return_type {
                #(#rebind_args_with_default_values)*
                let resolved_value = <Self as #trait_name>::#trait_field_name(
                    self,
                    executor,
                    #query_trail_arg
                    #(#arg_names,)*
                ) #awaitness #tryness;
                #return_result
            }
        };

        tokens.extend(code);
    }
}

#[derive(Debug)]
struct FieldToTokensForSubscriptionTrait<'a, 'doc> {
    field: &'a Field<'doc>,
}

impl<'a, 'doc> ToTokens for FieldToTokensForSubscriptionTrait<'a, 'doc> {
    fn to_tokens(&self, tokens: &mut TokenStream) {
        let Field {
            context_type,
            args,
            description: _,
            name: _,
            error_type: _,
            return_type: _,
            directives: _,
        } = self.field;

        let name = self.field.trait_field_name();
        let args = args.iter().map(|arg| arg.to_tokens_for_trait());
        let query_trail_param = self.field.query_trail_param();
        let asyncness = self.field.asyncness();
        let return_type = self.field.full_stream_return_type();

        tokens.extend(quote! {
            #asyncness fn #name<'s, 'r, 'a>(
                &'s self,
                executor: &juniper_from_schema::juniper::Executor<'r, 'a, #context_type>,
                #query_trail_param
                #(#args,)*
            ) -> #return_type;
        });
    }
}

#[derive(Debug)]
struct FieldArg<'doc> {
    name: Ident,
    name_without_raw_ident: Ident,
    description: Option<&'doc String>,
    ty: Type,
    default_value: Option<TokenStream>,
}

impl<'doc> FieldArg<'doc> {
    fn to_tokens_for_graphql_object_impl<'a>(&'a self) -> FieldArgToTokensGraphqlObject<'a, 'doc> {
        FieldArgToTokensGraphqlObject(self)
    }

    fn to_tokens_for_trait<'a>(&'a self) -> FieldArgsToTokensTrait<'a, 'doc> {
        FieldArgsToTokensTrait(self)
    }

    fn to_tokens_for_interface<'a>(&'a self) -> FieldArgsToTokensInterface<'a, 'doc> {
        FieldArgsToTokensInterface(self)
    }
}

struct FieldArgToTokensGraphqlObject<'a, 'doc>(&'a FieldArg<'doc>);

impl<'a, 'doc> ToTokens for FieldArgToTokensGraphqlObject<'a, 'doc> {
    fn to_tokens(&self, tokens: &mut TokenStream) {
        let FieldArg {
            name,
            name_without_raw_ident: _,
            description: _,
            ty,
            default_value: _,
        } = self.0;

        tokens.extend(quote! {
            #name: #ty
        });
    }
}

struct FieldArgsToTokensTrait<'a, 'doc>(&'a FieldArg<'doc>);

impl<'a, 'doc> ToTokens for FieldArgsToTokensTrait<'a, 'doc> {
    fn to_tokens(&self, tokens: &mut TokenStream) {
        let FieldArg {
            name,
            name_without_raw_ident: _,
            description: _,
            ty,
            default_value,
        } = self.0;

        let ty = if default_value.is_some() {
            ty.remove_one_layer_of_nullability()
        } else {
            ty
        };

        tokens.extend(quote! {
            #name: #ty
        });
    }
}

struct FieldArgsToTokensInterface<'a, 'doc>(&'a FieldArg<'doc>);

impl<'a, 'doc> ToTokens for FieldArgsToTokensInterface<'a, 'doc> {
    fn to_tokens(&self, tokens: &mut TokenStream) {
        let FieldArg {
            name,
            description: _,
            name_without_raw_ident: _,
            ty,
            default_value: _,
        } = self.0;

        tokens.extend(quote! {
            #name: #ty
        });
    }
}

#[derive(Debug)]
struct Subscription<'doc> {
    name: Ident,
    description: Option<&'doc String>,
    context_type: &'doc syn::Type,
    fields: Vec<Field<'doc>>,
}

impl<'doc> ToTokens for Subscription<'doc> {
    fn to_tokens(&self, tokens: &mut TokenStream) {
        let Subscription {
            name,
            description,
            context_type,
            fields,
        } = self;

        let mut graphql_attrs = GraphqlAttr::new_subscription();
        graphql_attrs.push_key_value(format_ident!("Context"), context_type);

        if let Some(description) = description {
            graphql_attrs.push_key_value(format_ident!("description"), description);
        }
        graphql_attrs.push_key_value(
            format_ident!("Scalar"),
            quote! { juniper_from_schema::juniper::DefaultScalarValue },
        );

        let trait_name = fields_trait_name(name);

        let fields_for_impl = fields
            .iter()
            .map(|field| field.to_tokens_for_subscription_impl(&trait_name));

        let fields_for_trait = fields
            .iter()
            .map(|field| field.to_tokens_for_subscription_trait());

        let async_trait_attr = if fields.iter().any(|f| f.directives.r#async.value) {
            Some(quote! { #[juniper_from_schema::juniper::async_trait] })
        } else {
            None
        };

        tokens.extend(quote! {
            #graphql_attrs
            impl #name {
                #(#fields_for_impl)*
            }

            #async_trait_attr
            pub trait #trait_name {
                #(#fields_for_trait)*
            }
        });
    }
}

#[derive(Debug)]
struct Interface<'doc> {
    description: Option<&'doc String>,
    name: Ident,
    trait_name: Ident,
    fields: Vec<Field<'doc>>,
    implementors: Vec<Ident>,
    context_type: &'doc syn::Type,
}

impl<'doc> ToTokens for Interface<'doc> {
    fn to_tokens(&self, tokens: &mut TokenStream) {
        let Interface {
            description,
            name,
            trait_name: interface_trait_name,
            implementors,
            context_type,
            fields,
        } = self;

        let mut graphql_attrs = GraphqlAttr::new_interface_top_level();
        graphql_attrs.push_key_value(format_ident!("for"), quote! { [ #(#implementors),* ] });
        graphql_attrs.push_key_value(format_ident!("Context"), quote! { #context_type });
        graphql_attrs.push_key_value(
            format_ident!("Scalar"),
            quote! { juniper_from_schema::juniper::DefaultScalarValue },
        );
        graphql_attrs.push_key_value(format_ident!("enum"), name);

        let name_lit = syn::LitStr::new(&name.to_string(), proc_macro2::Span::call_site());
        graphql_attrs.push_key_value(format_ident!("name"), name_lit);

        if let Some(description) = description {
            graphql_attrs.push_key_value(format_ident!("description"), description);
        }

        let fields_for_impl = fields.iter().map(|field| field.to_tokens_for_interface());

        tokens.extend(quote! {
            #graphql_attrs
            pub trait #interface_trait_name {
                #(#fields_for_impl)*
            }
        });

        for implementor in implementors {
            let trait_name = fields_trait_name(implementor);

            let fields_for_impl = fields
                .iter()
                .map(|field| field.to_tokens_for_interface_impl(&trait_name));

            let graphql_attr = GraphqlAttr::new_interface_top_level();

            tokens.extend(quote! {
                #graphql_attr
                impl #interface_trait_name for #implementor {
                    #(#fields_for_impl)*
                }
            })
        }
    }
}

#[derive(Debug)]
struct Union<'doc> {
    name: Ident,
    variants: Vec<UnionVariant>,
    description: Option<&'doc String>,
    context_type: &'doc syn::Type,
}

impl<'doc> ToTokens for Union<'doc> {
    fn to_tokens(&self, tokens: &mut TokenStream) {
        let Union {
            name,
            variants,
            description,
            context_type,
        } = self;

        let mut graphql_attrs = GraphqlAttr::new();
        graphql_attrs.push_key_value(format_ident!("Context"), context_type);
        graphql_attrs.push_key_value(
            format_ident!("Scalar"),
            quote! { juniper_from_schema::juniper::DefaultScalarValue },
        );

        if let Some(description) = description {
            graphql_attrs.push_key_value(format_ident!("description"), description);
        }

        let from_impls = variants.iter().map(|variant| {
            let inner_ty = &variant.type_inside;
            let rust_variant = &variant.rust_name;
            quote! {
                impl std::convert::From<#inner_ty> for #name {
                    fn from(inner: #inner_ty) -> #name {
                        #name::#rust_variant(inner)
                    }
                }
            }
        });

        tokens.extend(quote! {
            #[derive(juniper_from_schema::juniper::GraphQLUnion)]
            #graphql_attrs
            pub enum #name {
                #(#variants,)*
            }

            #(#from_impls)*
        });
    }
}

#[derive(Debug)]
struct UnionVariant {
    rust_name: Ident,
    type_inside: Box<Type>,
}

impl ToTokens for UnionVariant {
    fn to_tokens(&self, tokens: &mut TokenStream) {
        let UnionVariant {
            rust_name,
            type_inside,
        } = self;

        tokens.extend(quote! {
            #rust_name(#type_inside)
        });
    }
}

#[derive(Debug)]
struct Enum<'doc> {
    name: Ident,
    variants: Vec<EnumVariant<'doc>>,
    description: Option<&'doc String>,
}

impl<'doc> ToTokens for Enum<'doc> {
    fn to_tokens(&self, tokens: &mut TokenStream) {
        let Enum {
            name,
            variants,
            description,
        } = self;

        let graphql_attr = description.map(|description| {
            quote! {
                #[graphql(description=#description)]
            }
        });

        let string_to_enum_value_mappings = variants.iter().map(|variant| {
            let graphql_name = variant.graphql_name;
            let variant_name = &variant.name;
            quote! { &#graphql_name => #name::#variant_name }
        });

        tokens.extend(quote! {
            #[derive(
                juniper_from_schema::juniper::GraphQLEnum,
                Debug,
                Eq,
                PartialEq,
                Ord,
                PartialOrd,
                Copy,
                Clone,
                Hash,
            )]
            #graphql_attr
            pub enum #name {
                #(#variants),*
            }

            impl<'a, 'b> query_trails::FromLookAheadValue<#name>
                for &'a juniper_from_schema::juniper::LookAheadValue<'b, juniper_from_schema::juniper::DefaultScalarValue>
            {
                fn from(self) -> #name {
                    match self {
                        juniper_from_schema::juniper::LookAheadValue::Enum(name) => {
                            match name {
                                #(#string_to_enum_value_mappings,)*
                                other => panic!("Invalid enum name: {}", other),
                            }
                        },
                        juniper_from_schema::juniper::LookAheadValue::Null => panic!(
                            "Failed converting look ahead value. Expected enum type got `null`",
                        ),
                        juniper_from_schema::juniper::LookAheadValue::List(_) => panic!(
                            "Failed converting look ahead value. Expected enum type got `list`",
                        ),
                        juniper_from_schema::juniper::LookAheadValue::Object(_) => panic!(
                            "Failed converting look ahead value. Expected enum type got `object`",
                        ),
                        juniper_from_schema::juniper::LookAheadValue::Scalar(_) => panic!(
                            "Failed converting look ahead value. Expected enum type got `scalar`",
                        ),
                    }
                }
            }
        });
    }
}

#[derive(Debug)]
struct EnumVariant<'doc> {
    name: Ident,
    deprecation: Deprecation,
    description: Option<&'doc String>,
    graphql_name: &'doc str,
}

impl<'doc> ToTokens for EnumVariant<'doc> {
    fn to_tokens(&self, tokens: &mut TokenStream) {
        let EnumVariant {
            name,
            description,
            deprecation,
            graphql_name,
        } = self;

        let mut graphql_attrs = GraphqlAttr::new();
        graphql_attrs.push_key_value(format_ident!("name"), graphql_name);

        match deprecation {
            Deprecation::NoDeprecation => {}
            Deprecation::Deprecated(None) => graphql_attrs.push(format_ident!("deprecated")),
            Deprecation::Deprecated(Some(reason)) => {
                graphql_attrs.push_key_value(format_ident!("deprecated"), reason)
            }
        };

        if let Some(description) = description {
            graphql_attrs.push_key_value(format_ident!("description"), description);
        }

        tokens.extend(quote! {
            #[allow(missing_docs)]
            #graphql_attrs
            #name
        })
    }
}

#[derive(Debug)]
struct InputObject<'doc> {
    name: Ident,
    description: Option<&'doc String>,
    fields: Vec<InputObjectField<'doc>>,
}

impl<'doc> ToTokens for InputObject<'doc> {
    fn to_tokens(&self, tokens: &mut TokenStream) {
        let InputObject {
            name,
            description,
            fields,
        } = self;

        let mut graphql_attrs = GraphqlAttr::new();
        if let Some(description) = description {
            graphql_attrs.push_key_value(format_ident!("description"), description);
        }

        let field_names = fields
            .iter()
            .map(|field| format_ident!("{}_temp", field.name))
            .collect::<Vec<_>>();

        let temp_field_setters = fields
            .iter()
            .map(|field| {
                let name = LitStr::new(&field.name.to_string(), Span::call_site());
                let temp_name = format_ident!("{}_temp", field.name);
                let rust_type = &field.ty;
                quote! {
                    #name => {
                        #temp_name = Some(
                            query_trails::FromLookAheadValue::<#rust_type>::from(
                                look_ahead_value
                            )
                        );
                    },
                }
            })
            .collect::<Vec<_>>();

        let field_setters = fields
            .iter()
            .map(|field| {
                let name = &field.name;
                let temp_name = format_ident!("{}_temp", &field.name);
                quote! {
                    #name: #temp_name.unwrap_or_else(|| panic!("Field `{}` was not set", stringify!(#name))),
                }
            })
            .collect::<Vec<_>>();

        tokens.extend(quote! {
            #[derive(juniper_from_schema::juniper::GraphQLInputObject, Clone, Debug)]
            #graphql_attrs
            pub struct #name {
                #(#fields),*
            }

            impl<'a, 'b> query_trails::FromLookAheadValue<#name>
                for &'a juniper_from_schema::juniper::LookAheadValue<'b, juniper_from_schema::juniper::DefaultScalarValue>
            {
                fn from(self) -> #name {
                    match self {
                        juniper_from_schema::juniper::LookAheadValue::Object(pairs) => {
                            #(
                                let mut #field_names = None;
                            )*
                            for (look_ahead_key, look_ahead_value) in pairs {
                                match *look_ahead_key {
                                    #(#temp_field_setters)*
                                    other => panic!("Invalid input object key: {}", other),
                                }
                            }
                            #name {
                                #(#field_setters)*
                            }
                        },
                        juniper_from_schema::juniper::LookAheadValue::Enum(_) => panic!(
                            "Failed converting look ahead value. Expected object type got `enum`",
                        ),
                        juniper_from_schema::juniper::LookAheadValue::Null => panic!(
                            "Failed converting look ahead value. Expected object type got `null`",
                        ),
                        juniper_from_schema::juniper::LookAheadValue::List(_) => panic!(
                            "Failed converting look ahead value. Expected object type got `list`",
                        ),
                        juniper_from_schema::juniper::LookAheadValue::Scalar(_) => panic!(
                            "Failed converting look ahead value. Expected object type got `scalar`",
                        ),
                    }
                }
            }
        });
    }
}

#[derive(Debug)]
struct InputObjectField<'doc> {
    name: Ident,
    ty: Type,
    description: Option<&'doc String>,
}

impl<'doc> ToTokens for InputObjectField<'doc> {
    fn to_tokens(&self, tokens: &mut TokenStream) {
        let InputObjectField {
            name,
            ty,
            description,
        } = self;

        let mut graphql_attrs = GraphqlAttr::new();
        if let Some(description) = description {
            graphql_attrs.push_key_value(format_ident!("description"), description);
        }

        tokens.extend(quote! {
            #graphql_attrs
            pub #name: #ty
        })
    }
}

#[derive(Debug)]
struct SchemaType {
    query_type: syn::Type,
    mutation_type: syn::Type,
    subscription_type: syn::Type,
}

impl ToTokens for SchemaType {
    fn to_tokens(&self, tokens: &mut TokenStream) {
        let SchemaType {
            query_type,
            mutation_type,
            subscription_type,
        } = self;

        tokens.extend(quote! {
            /// The GraphQL schema type generated by `juniper-from-schema`.
            pub type Schema = juniper_from_schema::juniper::RootNode<
                'static,
                #query_type,
                #mutation_type,
                #subscription_type,
            >;
        });
    }
}

fn add_deprecation_graphql_attr_token(
    directives: &FieldDirectives,
    graphql_attrs: &mut GraphqlAttr,
) {
    if let Some(Deprecation::Deprecated(reason)) = &directives.deprecated {
        if let Some(reason) = reason {
            graphql_attrs.push_key_value(format_ident!("deprecated"), reason);
        } else {
            graphql_attrs.push(format_ident!("deprecated"));
        }
    }
}

#[derive(Debug)]
enum GraphqlAttr {
    Normal { items: Vec<GraphqlAttrItem> },
    Object { items: Vec<GraphqlAttrItem> },
    Interface { items: Vec<GraphqlAttrItem> },
    Subscription { items: Vec<GraphqlAttrItem> },
}

#[derive(Debug)]
enum GraphqlAttrItem {
    Bare(Ident),
    KeyValue { key: Ident, value: TokenStream },
    Fn { name: Ident, args: Vec<TokenStream> },
}

impl GraphqlAttr {
    fn new() -> Self {
        Self::Normal { items: Vec::new() }
    }

    fn new_object() -> Self {
        Self::Object { items: Vec::new() }
    }

    fn new_subscription() -> Self {
        Self::Subscription { items: Vec::new() }
    }

    fn new_interface_top_level() -> Self {
        Self::Interface { items: Vec::new() }
    }

    fn push(&mut self, key: Ident) {
        let items = match self {
            GraphqlAttr::Normal { items } => items,
            GraphqlAttr::Object { items } => items,
            GraphqlAttr::Interface { items } => items,
            GraphqlAttr::Subscription { items } => items,
        };
        items.push(GraphqlAttrItem::Bare(key));
    }

    fn push_key_value<T: ToTokens>(&mut self, key: Ident, value: T) {
        let items = match self {
            GraphqlAttr::Normal { items } => items,
            GraphqlAttr::Object { items } => items,
            GraphqlAttr::Interface { items } => items,
            GraphqlAttr::Subscription { items } => items,
        };
        items.push(GraphqlAttrItem::KeyValue {
            key,
            value: quote! { #value },
        });
    }

    fn push_fn<T, I>(&mut self, name: Ident, values: I)
    where
        T: ToTokens,
        I: Iterator<Item = T>,
    {
        let items = match self {
            GraphqlAttr::Normal { items } => items,
            GraphqlAttr::Object { items } => items,
            GraphqlAttr::Interface { items } => items,
            GraphqlAttr::Subscription { items } => items,
        };
        let args = values
            .map(|value| {
                quote! { #value }
            })
            .collect();
        items.push(GraphqlAttrItem::Fn { name, args });
    }
}

impl ToTokens for GraphqlAttr {
    fn to_tokens(&self, tokens: &mut TokenStream) {
        let (name, items) = match self {
            GraphqlAttr::Normal { items } => (quote! { graphql }, items),
            GraphqlAttr::Object { items } => (
                quote! { juniper_from_schema::juniper::graphql_object },
                items,
            ),
            GraphqlAttr::Interface { items } => (
                quote! { juniper_from_schema::juniper::graphql_interface },
                items,
            ),
            GraphqlAttr::Subscription { items } => (
                quote! { juniper_from_schema::juniper::graphql_subscription },
                items,
            ),
        };

        let code = quote! { #[ #name ( #(#items),* )] };
        tokens.extend(code);
    }
}

impl ToTokens for GraphqlAttrItem {
    fn to_tokens(&self, tokens: &mut TokenStream) {
        let new_tokens = match self {
            GraphqlAttrItem::Bare(k) => quote! { #k },
            GraphqlAttrItem::KeyValue { key, value } => quote! { #key = #value },
            GraphqlAttrItem::Fn { name, args } => quote! { #name ( #(#args),* ) },
        };
        tokens.extend(new_tokens);
    }
}

#[derive(Debug, Copy, Clone)]
enum FieldLocation {
    Object,
    Interface,
    Subscription,
}

fn maybe_wrap_final_return_type_in_result(
    ty: syn::Type,
    error_type: &syn::Type,
    directives: &FieldDirectives,
) -> syn::Type {
    if directives.infallible.value {
        ty
    } else {
        parse_quote! {
            std::result::Result<#ty, #error_type>
        }
    }
}
