use super::*;

fn launcher_fixture() -> Result<(tempfile::TempDir, PathBuf, PathBuf)> {
    let temp = tempfile::tempdir()?;
    let proton = temp.path().join("selected Proton");
    fs::create_dir_all(proton.join("files/bin"))?;
    fs::write(proton.join("proton"), b"must not execute")?;
    fs::write(proton.join("files/bin/wine"), b"must not execute")?;
    let exe = temp.path().join("Game with spaces/Game.exe");
    fs::create_dir_all(exe.parent().context("fixture directory")?)?;
    fs::write(&exe, b"owned game fixture")?;
    Ok((temp, proton, exe))
}

#[test]
fn faugus_umu_wrapper_preserves_runner_prefix_and_literal_arguments() -> Result<()> {
    let (temp, proton, exe) = launcher_fixture()?;
    let prefix = temp.path().join("existing prefix");
    let vars: BTreeMap<_, OsString> = [
        ("PROTONPATH", proton.clone().into_os_string()),
        ("WINEPREFIX", prefix.clone().into_os_string()),
    ]
    .into();
    let args = [
        "mangohud".into(),
        "/launcher/umu-run".into(),
        exe.clone().into_os_string(),
        "literal; $(touch do-not-create)".into(),
        "".into(),
        "second.exe".into(),
    ];
    let request = parse_with_env(&args, &|key| vars.get(key).cloned())?;
    assert_eq!(request.proton, proton);
    assert_eq!(request.executable, exe);
    assert_eq!(request.prefix, Some(prefix.clone()));
    assert_eq!(
        request.arguments,
        ["literal; $(touch do-not-create)", "", "second.exe"]
    );
    assert!(request.mangohud);
    assert!(!prefix.exists());
    Ok(())
}

#[test]
fn lutris_selected_wine_resolves_its_proton_before_an_umu_token() -> Result<()> {
    let (_temp, proton, exe) = launcher_fixture()?;
    let vars: BTreeMap<_, OsString> = [
        ("PROTONPATH", "GE-Proton".into()),
        ("WINE", proton.join("files/bin/wine").into_os_string()),
    ]
    .into();
    let args = ["/lutris/runtime/umu/umu-run".into(), exe.into_os_string()];
    let request = parse_with_env(&args, &|key| vars.get(key).cloned())?;
    assert_eq!(request.proton, proton);
    Ok(())
}

#[test]
fn direct_proton_wine_command_needs_no_extra_proton_option() -> Result<()> {
    let (_temp, proton, exe) = launcher_fixture()?;
    let args = [
        proton.join("files/bin/wine").into_os_string(),
        exe.into_os_string(),
    ];
    let request = parse_with_env(&args, &|_| None)?;
    assert_eq!(request.proton, proton);
    Ok(())
}

#[test]
fn named_installed_runner_resolves_exactly_without_picking_a_different_version() -> Result<()> {
    let (temp, proton, exe) = launcher_fixture()?;
    let data = temp.path().join("launcher data");
    fs::create_dir_all(data.join("Steam/compatibilitytools.d"))?;
    std::os::unix::fs::symlink(
        &proton,
        data.join("Steam/compatibilitytools.d/Selected Latest"),
    )?;
    let vars: BTreeMap<_, OsString> = [
        ("PROTONPATH", "Selected Latest".into()),
        ("HOME", temp.path().as_os_str().to_owned()),
        ("XDG_DATA_HOME", data.into_os_string()),
    ]
    .into();
    let args = ["umu-run".into(), exe.into_os_string()];
    let request = parse_with_env(&args, &|key| vars.get(key).cloned())?;
    assert_eq!(request.proton, proton);
    Ok(())
}

#[test]
fn malformed_launcher_and_setup_commands_fail_before_launching_anything() -> Result<()> {
    let (temp, proton, exe) = launcher_fixture()?;
    let env = |name: &str| (name == "PROTONPATH").then(|| proton.clone().into_os_string());
    let duplicate = [
        "--proton".into(),
        proton.clone().into_os_string(),
        "--".into(),
        exe.clone().into_os_string(),
        "/umu/umu-run".into(),
        exe.into_os_string(),
    ];
    assert!(parse_with_env(&duplicate, &env).is_err());
    for args in [
        vec!["umu-run".into(), "--config".into(), "game.toml".into()],
        vec!["umu-run".into(), "winetricks".into()],
    ] {
        assert!(parse_with_env(&args, &env).is_err());
    }
    let unresolved: BTreeMap<_, OsString> = [
        ("PROTONPATH", "GE-Proton".into()),
        ("HOME", temp.path().as_os_str().to_owned()),
    ]
    .into();
    let error = parse_with_env(&["umu-run".into(), "Game.exe".into()], &|key| {
        unresolved.get(key).cloned()
    })
    .err()
    .context("unresolved runtime must fail")?
    .to_string();
    assert!(error.contains("has not resolved Proton selection"));
    Ok(())
}
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
