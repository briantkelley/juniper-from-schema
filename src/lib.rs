// #![deny(unused_imports, dead_code, unused_variables)]
#![recursion_limit="128"]

extern crate proc_macro;
extern crate proc_macro2;

#[macro_use]
mod macros;
mod nullable_type;
mod walk_ast;

use self::walk_ast::{find_special_scalar_types, gen_juniper_code, gen_query_trails, Output};
use graphql_parser::parse_schema;
use proc_macro2::TokenStream;

#[proc_macro]
pub fn graphql_schema_from_file(input: proc_macro::TokenStream) -> proc_macro::TokenStream {
    let input: TokenStream = input.into();

    let file = input.to_string().replace("\"", "");
    let pwd = std::env::current_dir().unwrap();
    let path = pwd.join(file);

    match read_file(&path) {
        Ok(schema) => parse_and_gen_schema(schema),
        Err(err) => panic!("{}", err),
    }
}

#[proc_macro]
pub fn graphql_schema(input: proc_macro::TokenStream) -> proc_macro::TokenStream {
    let input: TokenStream = input.into();
    let schema = input.to_string();
    parse_and_gen_schema(schema)
}

fn parse_and_gen_schema(schema: String) -> proc_macro::TokenStream {
    let doc = match parse_schema(&schema) {
        Ok(doc) => doc,
        Err(parse_error) => panic!("{}", parse_error),
    };

    let special_scalars = find_special_scalar_types(&doc);

    let mut output = Output::new(special_scalars);

    gen_query_trails(&doc, &mut output);
    gen_juniper_code(doc, &mut output);

    output.tokens().into_iter().collect::<TokenStream>().into()
}

fn read_file(path: &std::path::PathBuf) -> Result<String, std::io::Error> {
    use std::{fs::File, io::prelude::*};
    let mut file = File::open(path)?;
    let mut contents = String::new();
    file.read_to_string(&mut contents)?;
    Ok(contents)
}