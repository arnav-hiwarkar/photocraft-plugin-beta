use crate::Error;

pub const FILL_ID: &str = "org.example.corpus.fill";
pub const MANIFEST_ID: &str = "org.example.corpus.manifest";
pub const ABSENT_ID: &str = "org.example.corpus.absent";

pub struct Module {
    pub text: String,
    pub binary: Vec<u8>,
}

const FILL_BODY: &str = "(local $i i32)
    (block $done
      (loop $each
        (br_if $done (i32.ge_u (local.get $i) (local.get $len)))
        (f32.store (i32.add (local.get $buf) (local.get $i)) (f32.const 0.25))
        (local.set $i (i32.add (local.get $i) (i32.const 4)))
        (br $each)))
    (i32.const 0)";

const KEEP_BODY: &str = "(i32.const 0)";

fn module(manifest: &serde_json::Value, filter: &str) -> Result<Module, Error> {
    let json = serde_json::to_string(manifest).map_err(|e| Error::Module(e.to_string()))?;
    let text = format!(
        r#"(module
  (memory (export "memory") 1)
  (data (i32.const 16) "{data}")
  (func (export "pc_abi_version") (result i32)
    (i32.const 1))
  (func (export "pc_manifest") (result i64)
    (i64.or (i64.shl (i64.const {len}) (i64.const 32)) (i64.const 16)))
  (func (export "pc_alloc") (param $n i32) (result i32)
    (local $old i32)
    (local.set $old (memory.grow (i32.shr_u (i32.add (local.get $n) (i32.const 65535)) (i32.const 16))))
    (if (result i32) (i32.eq (local.get $old) (i32.const -1))
      (then (i32.const 0))
      (else (i32.mul (local.get $old) (i32.const 65536)))))
  (func (export "pc_filter")
    (param $buf i32) (param $len i32) (param $w i32) (param $h i32)
    (param $ch i32) (param $fmt i32) (param $pp i32) (param $pl i32) (result i32)
    {filter}))
"#,
        data = json.replace('\\', "\\\\").replace('"', "\\\""),
        len = json.len(),
    );
    let binary = wat::parse_str(&text).map_err(|e| Error::Module(e.to_string()))?;
    Ok(Module { text, binary })
}

pub fn fill() -> Result<Module, Error> {
    module(
        &serde_json::json!({
            "id": FILL_ID,
            "name": "Corpus Fill",
            "version": "1.0.0",
            "kind": "filter",
            "description": "Sets every sample to 0.25.",
            "author": "PhotoCraft legacy corpus",
        }),
        FILL_BODY,
    )
}

pub fn manifest() -> Result<Module, Error> {
    module(
        &serde_json::json!({
            "id": MANIFEST_ID,
            "name": "Corpus Manifest",
            "version": "2.1.0",
            "kind": "filter",
            "description": "Declares one parameter of each type.",
            "author": "PhotoCraft legacy corpus",
            "params": {
                "radius": {"type": "number", "min": 0, "max": 50, "default": 4, "unit": "px"},
                "steps": {"type": "int", "min": 1, "max": 9, "default": 3},
                "invert": {"type": "bool", "default": false},
                "edge": {"type": "choice", "options": ["clamp", "wrap", "mirror"], "default": "wrap"},
            },
            "overlap": 2,
            "area": "canvas",
        }),
        KEEP_BODY,
    )
}
