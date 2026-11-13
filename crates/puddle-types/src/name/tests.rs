// SPDX-License-Identifier: GPL-3.0-or-later
use super::*;

#[test]
fn checkout_directories_follow_git_or_fall_back() {
    for (url, dir) in [
        ("https://github.com/acme/api.git", "api"),
        ("https://github.com/acme/api", "api"),
        ("https://github.com/acme/api/", "api"),
        ("git@github.com:acme/web.git", "web"),
        ("https://h/x/my_repo.v2", "my_repo.v2"),
        ("https://example.org/", "example.org"),
        ("https://h/x/.hidden", "src"),
        ("https://h/x/-opt", "src"),
        ("https://h/x/a%20b", "src"),
        ("", "src"),
        ("/", "src"),
        ("https://h/x/.git", "src"),
        ("https://h/x/lost+found", "src"),
    ] {
        assert_eq!(CheckoutDir::from_url(url).as_str(), dir, "{url}");
    }
    let long = format!("https://h/x/{}", "a".repeat(101));
    assert_eq!(CheckoutDir::from_url(&long).as_str(), "src");
}

#[test]
fn a_checkout_directory_is_one_plain_segment() {
    assert_eq!(CheckoutDir::new("api-2").unwrap().as_str(), "api-2");
    assert!(CheckoutDir::new(&"x".repeat(100)).is_ok());
    for (bad, why) in [
        ("", "empty"),
        (".", "start with"),
        ("..", "start with"),
        (".puddle", "start with"),
        ("-x", "start with"),
        ("lost+found", "lost+found"),
        ("a/b", "ASCII letters"),
        ("a b", "ASCII letters"),
        ("é", "ASCII letters"),
        (&"x".repeat(101), "100"),
    ] {
        let err = CheckoutDir::new(bad).unwrap_err();
        assert!(err.to_string().contains(why), "{bad:?}: {err}");
    }
    assert!(serde_json::from_str::<CheckoutDir>(r#""../etc""#).is_err());
    assert_eq!(
        serde_json::from_str::<CheckoutDir>(r#""web""#).unwrap(),
        CheckoutDir::new("web").unwrap()
    );
}

#[test]
fn a_workspaces_sandbox_has_its_name_and_helper_sandboxes_have_none_of_a_workspace() {
    let w = WorkspaceId::new("acme").unwrap();
    let sandbox = w.sandbox_name();
    assert_eq!(sandbox.as_str(), "acme");
    assert_eq!(sandbox.workspace_id(), Some(w));
    // A helper sandbox's name is valid as a sandbox name; a reserved one is no workspace's.
    let helper = SandboxName::new("m--acme").unwrap();
    assert_eq!(helper.workspace_id().unwrap().as_str(), "m--acme");
    let reserved = SandboxName::new("tauri").unwrap();
    assert_eq!(reserved.workspace_id(), None);
    let err = SandboxName::new("Acme").unwrap_err();
    assert_eq!(err.what(), "sandbox name");
    assert_eq!(serde_json::to_string(&sandbox).unwrap(), r#""acme""#);
    assert!(serde_json::from_str::<SandboxName>(r#""A""#).is_err());
}

#[test]
fn reserved_workspace_names_are_no_workspace_id_either() {
    for reserved in RESERVED_WORKSPACE_NAMES {
        let err = WorkspaceId::new(reserved).unwrap_err();
        assert_eq!(err.what(), "workspace id");
        assert!(err.reason().contains("reserved"), "{err}");
    }
    assert!(WorkspaceId::new("tauri-app").is_ok());
}

#[test]
fn an_entry_id_takes_what_a_generated_id_looks_like_and_nothing_else() {
    for bytes in [[0; 5], [0xff; 5], [0x12, 0x34, 0x56, 0x78, 0x9a]] {
        let id = EntryId::from_random(bytes);
        assert_eq!(EntryId::new(id.as_str()).unwrap(), id);
        assert_eq!(id.as_str(), WorkspaceId::from_random(bytes).as_str());
    }
    for bad in [
        "",
        "api",
        "shop-api",
        "0aaaaaaa",
        "A0000000",
        "a000000",
        "a00000000",
        "i0000000",
        "a000000u",
    ] {
        assert!(EntryId::new(bad).is_err(), "{bad:?}");
    }
}

#[test]
fn every_id_from_random_bits_is_eight_crockford_characters_starting_with_a_letter() {
    let letters = "abcdefghjkmnpqrstvwxyz";
    // Every first five bits, with the rest of the bits varied.
    for seed in 0_u8..=255 {
        let bytes = [
            seed,
            seed.wrapping_mul(7),
            !seed,
            seed ^ 0x5a,
            seed.rotate_left(3),
        ];
        let id = WorkspaceId::from_random(bytes);
        let s = id.as_str();
        assert_eq!(s.len(), WorkspaceId::GENERATED_LEN, "{s}");
        assert!(s.chars().all(|c| CROCKFORD.contains(c)), "{s}");
        assert!(s.starts_with(|c| letters.contains(c)), "{s}");
        assert!(!s.contains(['i', 'l', 'o', 'u', '-']), "{s}");
        // Every generated id is a valid id, and so a volume and a sandbox name.
        assert_eq!(WorkspaceId::new(s).unwrap(), id);
        assert!(!RESERVED_WORKSPACE_NAMES.contains(&s));
    }
}

#[test]
fn exact_ids_for_fixed_bytes_pin_every_byte_position() {
    let cases: [([u8; 5], &str); 6] = [
        ([0, 0, 0, 0, 1], "a0000001"),
        ([0, 0, 0, 1, 0], "a0000080"),
        ([0, 0, 1, 0, 0], "a0002000"),
        ([0, 1, 0, 0, 0], "a00g0000"),
        ([1, 0, 0, 0, 0], "a4000000"),
        ([0xde, 0xad, 0xbe, 0xef, 0x42], "ftpvxvt2"),
    ];
    for (bytes, id) in cases {
        assert_eq!(WorkspaceId::from_random(bytes).as_str(), id, "{bytes:?}");
    }
}

#[test]
fn every_five_bits_pick_their_own_character() {
    assert_eq!(CROCKFORD.len(), 32);
    assert_eq!(LETTERS, 22);
    // The groups 0 to 7, five bits each: 00000 00001 00010 00011 00100 00101 00110 00111.
    let one_each = [0x00, 0x44, 0x32, 0x14, 0xc7];
    assert_eq!(WorkspaceId::from_random(one_each).as_str(), "a1234567");
    // The first five bits run over the letters, wrapping after the 22nd.
    let firsts: String = (0_u8..32)
        .map(|five| WorkspaceId::from_random([five << 3, 0, 0, 0, 0]))
        .map(|id| id.as_str().chars().next().unwrap())
        .collect();
    assert_eq!(firsts, "abcdefghjkmnpqrstvwxyzabcdefghjk");
    // The last character takes the last five bits.
    let lasts: String = (0_u8..32)
        .map(|five| WorkspaceId::from_random([0, 0, 0, 0, five]))
        .map(|id| id.as_str().chars().last().unwrap())
        .collect();
    assert_eq!(lasts, CROCKFORD);
}

#[test]
fn workspace_id_leaves_room_for_the_volume_prefix() {
    let longest = "w".repeat(WorkspaceId::MAX_LEN);
    let id = WorkspaceId::new(&longest).unwrap();
    assert_eq!(id.volume_name().as_str().len(), 63);
    assert!(VolumeName::new(id.volume_name().as_str()).is_ok());
    assert!(WorkspaceId::new(&"w".repeat(WorkspaceId::MAX_LEN + 1)).is_err());
}

#[test]
fn volume_name_maps_back_to_its_workspace() {
    let id = WorkspaceId::new("acme").unwrap();
    assert_eq!(id.volume_name().workspace_id(), Some(id));
    assert_eq!(VolumeName::new("other").unwrap().workspace_id(), None);
    assert_eq!(
        VolumeName::new("ws-").ok(),
        None,
        "trailing '-' isn't a label"
    );
    assert!(VolumeName::new("Bad").is_err());
}

#[test]
fn image_ref_checks_shape_only() {
    assert!(ImageRef::new("mcr.microsoft.com/devcontainers/base:debian").is_ok());
    assert!(ImageRef::new("alpine@sha256:0123").is_ok());
    assert!(ImageRef::new("").is_err());
    assert!(ImageRef::new("a b").is_err());
    assert!(ImageRef::new("a\nb").is_err());
    assert!(ImageRef::new(&"a".repeat(513)).is_err());
}

#[test]
fn string_conversions_round_trip() {
    let w: WorkspaceId = "w".parse().unwrap();
    assert_eq!(w.to_string(), "w");
    assert_eq!(AsRef::<str>::as_ref(&w), "w");
    assert_eq!(String::from(w.clone()), "w");
    assert_eq!(WorkspaceId::try_from("w").unwrap(), w);
    assert_eq!(WorkspaceId::try_from(String::from("w")).unwrap(), w);
    let v: VolumeName = "v".parse().unwrap();
    assert_eq!(v.as_str(), "v");
    let i: ImageRef = "alpine".parse().unwrap();
    assert_eq!(i.as_str(), "alpine");
}

#[test]
fn serde_validates_on_the_way_in() {
    let n: WorkspaceId = serde_json::from_str(r#""box""#).unwrap();
    assert_eq!(serde_json::to_string(&n).unwrap(), r#""box""#);
    assert!(serde_json::from_str::<WorkspaceId>(r#""Box""#).is_err());
    assert!(serde_json::from_str::<WorkspaceId>(r#""ipc""#).is_err());
    assert!(serde_json::from_str::<WorkspaceId>(r#""-""#).is_err());
    assert!(serde_json::from_str::<VolumeName>(r#""""#).is_err());
    assert!(serde_json::from_str::<ImageRef>(r#""a b""#).is_err());
}

#[test]
fn an_image_ref_literal_is_the_same_image_ref_as_a_parsed_one() {
    let image = "mcr.microsoft.com/devcontainers/base:debian";
    assert_eq!(
        ImageRef::from(ImageRefLit::new(image)),
        ImageRef::new(image).unwrap()
    );
}

#[test]
#[should_panic(expected = "an image reference is")]
fn an_invalid_image_ref_literal_panics_which_stops_a_const_build() {
    let _ = ImageRefLit::new("a b");
}

#[test]
#[should_panic(expected = "an image reference is")]
fn an_empty_image_ref_literal_panics_which_stops_a_const_build() {
    let _ = ImageRefLit::new("");
}
