# [juniper-from-schema](https://crates.io/crates/juniper-from-schema)

![Build](https://github.com/davidpdrsn/juniper-from-schema/workflows/Build/badge.svg)

This library contains a procedural macro that reads a GraphQL schema file, and generates the
corresponding [Juniper](https://crates.io/crates/juniper) [macro calls]. This means you can
have a real schema file and be guaranteed that it matches your Rust implementation. It also
removes most of the boilerplate involved in using Juniper.

[macro calls]: https://graphql-rust.github.io/types/objects/complex_fields.html

# Example

Imagine you have a GraphQL schema like this:

```graphql
schema {
  query: Query
}

type Query {
  helloWorld(name: String!): String! @juniper(ownership: "owned")
}
```

That can be implemented like so:

```rust
use juniper_from_schema::graphql_schema_from_file;

// This is the important line
graphql_schema_from_file!("readme_schema.graphql");

pub struct Context;

impl juniper::Context for Context {}

pub struct Query;

// This trait is generated by `graphql_schema_from_file!` based on the schema
impl QueryFields for Query {
    fn field_hello_world(
        &self,
        executor: &juniper::Executor<'_, Context>,
        name: String,
    ) -> juniper::FieldResult<String> {
        Ok(format!("Hello, {}!", name))
    }
}

pub struct Mutation;

// This trait is generated by `graphql_schema_from_file!` based on the schema
impl MutationFields for Mutation {
    fn field_noop(
      &self,
      executor: &juniper::Executor<'_, Context>,
    ) -> juniper::FieldResult<&bool> {
        Ok(&true)
    }
}

fn main() {
    let ctx = Context;

    let query = "query { helloWorld(name: \"Ferris\") }";

    let (result, errors) = juniper::execute(
        query,
        None,
        &Schema::new(Query, Mutation),
        &juniper::Variables::new(),
        &ctx,
    )
    .unwrap();

    assert_eq!(errors.len(), 0);
    assert_eq!(
        result
            .as_object_value()
            .unwrap()
            .get_field_value("helloWorld")
            .unwrap()
            .as_scalar_value::<String>()
            .unwrap(),
        "Hello, Ferris!",
    );
}
```

See the [crate documentation](https://docs.rs/juniper-from-schema/) for a usage examples and more info.

# N+1s

If you're having issues with N+1 query bugs consider using [juniper-eager-loading](https://crates.io/crates/juniper-eager-loading). It was built to integrate seamlessly with juniper-from-schema.
