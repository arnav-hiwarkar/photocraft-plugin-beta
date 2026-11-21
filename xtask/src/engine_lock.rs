pub fn run(args: &[&str]) -> Result<(), String> {
    let mut cmd = crate::cargo();
    cmd.args(["test", "-p", "photocraft-engine", "--test", "it", "engine_lock::engine_lock_lists_every_shipped_id", "--", "--exact"]);
    cmd.env("CRAFT_ENGINE_LOCK_ARGS", args.join("\n"));
    crate::run(cmd, &format!("engine-lock {}", args.join(" ")))
}
