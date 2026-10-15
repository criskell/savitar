use std::path::Path;

fn manifest(crate_name: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join(crate_name)
        .join("Cargo.toml");
    std::fs::read_to_string(&path).unwrap_or_else(|error| {
        panic!("could not read {}: {error}", path.display());
    })
}

#[test]
fn the_soundness_layer_never_depends_on_a_concrete_provider() {
    let text = manifest("savitar-sound");
    assert!(
        !text.contains("tsapi"),
        "savitar-sound must reach a type provider through the savitar-typeinfo trait only"
    );
    assert!(
        !text.contains("goport") && !text.contains("ts_goport") && !text.contains("ts-rust"),
        "no external checker crate may cross savitar-typeinfo"
    );
    assert!(
        text.contains("savitar-typeinfo.workspace = true"),
        "savitar-sound should depend on the provider trait"
    );
}

#[test]
fn the_provider_boundary_crate_stays_free_of_the_concrete_provider() {
    let text = manifest("savitar-typeinfo");
    assert!(
        !text.contains("tsapi") && !text.contains("goport") && !text.contains("serde_json"),
        "savitar-typeinfo defines the boundary and must not know any wire format"
    );
}
