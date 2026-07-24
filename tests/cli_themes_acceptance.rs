use std::process::Command;

#[test]
fn themes_list_outputs_builtin_contract_as_json() {
    let output = Command::new(env!("CARGO_BIN_EXE_gander"))
        .args(["themes", "list"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let names: Vec<_> = value["themes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|theme| theme["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        names,
        [
            "gander",
            "catppuccin",
            "gruvbox",
            "solarized",
            "nord",
            "tokyo-night",
            "dracula"
        ]
    );
}
