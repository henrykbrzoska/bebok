use std::fmt::Display;

#[derive(Serialize, Deserialize)]
pub struct Bar {
    pub name: String,
    pub value: i32,
}

impl Display for Foo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Foo")
    }
}

pub fn baz() -> Result<(), String> {
    Ok(())
}

pub fn simple() {
    println!("hello");
}

#[test]
fn test_something() {
    assert_eq!(1 + 1, 2);
}

mod inner {
    pub fn helper() -> bool {
        true
    }
}

type MyType = std::collections::HashMap<String, i32>;

const MAX_SIZE: usize = 1024;

static COUNTER: i32 = 0;

use std::collections::HashMap;
