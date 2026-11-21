use crate::Error;

pub const FILL_ID: &str = "org.example.corpus.fill";
pub const MANIFEST_ID: &str = "org.example.corpus.manifest";
pub const ABSENT_ID: &str = "org.example.corpus.absent";
pub const PROBE_ID: &str = "org.example.corpus.probe";
pub const COMMANDS_ID: &str = "org.example.corpus.commands";
pub const PREVIEW_ID: &str = "org.example.corpus.preview";

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

const PROBE_BODY: &str = "(local $i i32) (local $skip i32)
    (local.set $skip (i32.and (i32.shr_u (local.get $fmt) (i32.const 8)) (i32.const 1)))
    (call $put (local.get $buf) (local.get $len) (local.get $ch) (local.get $skip) (f32.convert_i32_u (local.get $w)))
    (call $put (local.get $buf) (local.get $len) (local.get $ch) (local.get $skip) (f32.convert_i32_u (local.get $h)))
    (call $put (local.get $buf) (local.get $len) (local.get $ch) (local.get $skip) (f32.convert_i32_u (local.get $ch)))
    (call $put (local.get $buf) (local.get $len) (local.get $ch) (local.get $skip) (f32.convert_i32_u (local.get $fmt)))
    (call $put (local.get $buf) (local.get $len) (local.get $ch) (local.get $skip) (f32.convert_i32_u (local.get $pl)))
    (block $done
      (loop $each
        (br_if $done (i32.ge_u (local.get $i) (local.get $pl)))
        (call $put (local.get $buf) (local.get $len) (local.get $ch) (local.get $skip)
          (f32.convert_i32_u (i32.load8_u (i32.add (local.get $pp) (local.get $i)))))
        (local.set $i (i32.add (local.get $i) (i32.const 1)))
        (br $each)))
    (i32.const 0)";

const PUT: &str = r#"(global $at (mut i32) (i32.const 0))
  (func $put (param $buf i32) (param $len i32) (param $ch i32) (param $skip i32) (param $v f32)
    (if (i32.and (local.get $skip) (i32.eq (i32.rem_u (global.get $at) (local.get $ch)) (i32.sub (local.get $ch) (i32.const 1))))
      (then (global.set $at (i32.add (global.get $at) (i32.const 1)))))
    (if (i32.lt_u (i32.mul (global.get $at) (i32.const 4)) (local.get $len))
      (then (f32.store (i32.add (local.get $buf) (i32.mul (global.get $at) (i32.const 4))) (local.get $v))))
    (global.set $at (i32.add (global.get $at) (i32.const 1))))"#;

fn module(manifest: &serde_json::Value, filter: &str) -> Result<Module, Error> {
    module_with(manifest, "", filter)
}

fn module_with(manifest: &serde_json::Value, helpers: &str, filter: &str) -> Result<Module, Error> {
    let json = serde_json::to_string(manifest).map_err(|e| Error::Module(e.to_string()))?;
    let text = format!(
        r#"(module
  (memory (export "memory") 1)
  (data (i32.const 16) "{data}"){helpers}
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
        helpers = if helpers.is_empty() { String::new() } else { format!("\n  {helpers}") },
    );
    let binary = wat::parse_str(&text).map_err(|e| Error::Module(e.to_string()))?;
    Ok(Module { text, binary })
}

pub fn fill() -> Result<Module, Error> {
    fill_as(FILL_ID, "Corpus Fill")
}

pub fn commands() -> Result<Module, Error> {
    fill_as(COMMANDS_ID, "Corpus Commands")
}

fn fill_as(id: &str, name: &str) -> Result<Module, Error> {
    module(
        &serde_json::json!({
            "id": id,
            "name": name,
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

pub fn probe() -> Result<Module, Error> {
    module_with(
        &serde_json::json!({
            "id": PROBE_ID,
            "name": "Corpus Probe",
            "version": "1.0.0",
            "kind": "filter",
            "description": "Writes what the host passes into the pixels.",
            "author": "PhotoCraft legacy corpus",
            "params": {
                "level": {"type": "number", "min": 0, "max": 1, "default": 0.25},
                "count": {"type": "int", "min": 1, "max": 9, "default": 3},
                "flag": {"type": "bool", "default": false},
                "mode": {"type": "choice", "options": ["a", "b", "c"], "default": "a"},
            },
            "area": "canvas",
        }),
        PUT,
        PROBE_BODY,
    )
}

pub fn preview() -> Result<Module, Error> {
    module(
        &serde_json::json!({
            "id": PREVIEW_ID,
            "name": "Corpus Preview",
            "version": "1.0.0",
            "kind": "filter",
            "description": "Declares pixel lengths for the reduced live preview.",
            "author": "PhotoCraft legacy corpus",
            "params": {
                "radius": {"type": "number", "min": 0, "max": 100, "default": 12, "unit": "px"},
                "size": {"type": "int", "min": 2, "max": 50, "default": 10, "unit": "px"},
                "amount": {"type": "number", "min": 0, "max": 100, "default": 50},
            },
        }),
        KEEP_BODY,
    )
}
