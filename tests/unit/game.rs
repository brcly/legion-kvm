use super::*;
#[test]
fn steam_command_keeps_selected_proton_and_game_argument_boundaries() -> Result<()> {
    let root = tempfile::tempdir()?;
    let proton = root.path().join("Proton with spaces");
    fs::create_dir_all(proton.join("files/bin"))?;
    fs::write(proton.join("files/bin/wine"), b"owned")?;
    fs::write(proton.join("proton"), b"owned")?;
    let game = root.path().join("A game");
    fs::create_dir_all(game.join("Engine"))?;
    fs::create_dir_all(game.join("bin"))?;
    let exe = game.join("bin/Game.exe");
    fs::write(&exe, b"owned")?;
    let args = vec![
        OsString::from("container-wrapper"),
        OsString::from("--"),
        proton.join("proton").into_os_string(),
        "waitforexitandrun".into(),
        exe.clone().into_os_string(),
        "one argument with spaces".into(),
    ];
    let r = parse(&args)?;
    assert_eq!(r.proton, proton);
    assert_eq!(r.executable, exe);
    assert_eq!(r.root, game);
    assert_eq!(r.arguments, ["one argument with spaces"]);
    Ok(())
}

fn vars(pairs: &[(&str, &str)]) -> impl Iterator<Item = (OsString, OsString)> {
    pairs
        .iter()
        .map(|(key, value)| (OsString::from(key), OsString::from(value)))
}

#[test]
fn guest_xdg_overrides_replace_the_plain_names() -> Result<()> {
    let home = Path::new("/home/legion");
    let forwarded = guest_xdg(
        home,
        vars(&[
            (
                "LEGION_GUEST_XDG_CONFIG_HOME",
                "/home/legion/profile/config",
            ),
            ("LEGION_GUEST_XDG_DATA_HOME", "/home/legion/profile/data"),
            ("LEGION_GUEST_XDG_STATE_HOME", "/home/legion/profile/state"),
            ("LEGION_GUEST_XDG_CACHE_HOME", "/home/legion/profile/cache"),
            ("XDG_CONFIG_HOME", "/tmp/ignored"),
            ("PATH", "/usr/bin"),
        ]),
    )?;
    assert_eq!(
        forwarded,
        [
            (
                "XDG_CONFIG_HOME".to_owned(),
                "/home/legion/profile/config".to_owned()
            ),
            (
                "XDG_DATA_HOME".to_owned(),
                "/home/legion/profile/data".to_owned()
            ),
            (
                "XDG_STATE_HOME".to_owned(),
                "/home/legion/profile/state".to_owned()
            ),
            (
                "XDG_CACHE_HOME".to_owned(),
                "/home/legion/profile/cache".to_owned()
            ),
        ]
    );
    Ok(())
}

#[test]
fn guest_xdg_rejects_paths_outside_home() {
    let home = Path::new("/home/legion");
    for value in [
        "/etc/legion",
        "/home/legion/../other",
        "/home/legion-other/config",
        "relative/config",
    ] {
        assert!(
            guest_xdg(home, vars(&[("LEGION_GUEST_XDG_CONFIG_HOME", value)])).is_err(),
            "{value} must be rejected"
        );
    }
}
