fn main() {
    for (name, schema) in opensks_contracts::schema_jsons().expect("generate schemas") {
        println!("== {name} ==\n{schema}");
    }
}
