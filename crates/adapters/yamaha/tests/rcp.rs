//! Pure wire layer: framing, tokenizer, encoding, reply parsing — against
//! lines captured from a live TF1 V4.55.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::arithmetic_side_effects
)]

use patchbay_yamaha::rcp::codec::{LineCodec, MAX_LINE};
use patchbay_yamaha::rcp::token::{Token, quote, tokenize};
use patchbay_yamaha::{
    Command, Line, ParamReply, PrmInfo, PrmType, RcpValue, SceneBank, SceneCurrent, SceneInfo,
    TF1_PRMINFO_JSON,
};

const PRMINFO: &str = include_str!("fixtures/tf1_prminfo.json");

fn lines(codec: &mut LineCodec) -> Vec<String> {
    let mut out = Vec::new();
    while let Some(l) = codec.next_line().unwrap() {
        out.push(l);
    }
    out
}

#[test]
fn codec_split_across_reads() {
    let mut c = LineCodec::new();
    c.push(b"OK get MIXER:Current/InCh/Fa");
    assert!(lines(&mut c).is_empty());
    c.push(b"der/Level 0 0 -1");
    assert!(lines(&mut c).is_empty());
    c.push(b"000\n");
    assert_eq!(
        lines(&mut c),
        ["OK get MIXER:Current/InCh/Fader/Level 0 0 -1000"]
    );
    assert_eq!(c.buffered(), 0);
}

#[test]
fn codec_many_lines_per_read_and_partial_tail() {
    let mut c = LineCodec::new();
    c.push(b"OK devinfo productname \"TF1\"\nNOTIFY set A 0 0 1\n\nERROR get Inv");
    assert_eq!(
        lines(&mut c),
        ["OK devinfo productname \"TF1\"", "NOTIFY set A 0 0 1", ""]
    );
    c.push(b"alidArgument\n");
    assert_eq!(lines(&mut c), ["ERROR get InvalidArgument"]);
}

#[test]
fn codec_strips_cr_and_split_crlf() {
    let mut c = LineCodec::new();
    c.push(b"OK scpmode keepalive 10000\r");
    assert!(lines(&mut c).is_empty());
    c.push(b"\nOK x\r\r\n");
    assert_eq!(lines(&mut c), ["OK scpmode keepalive 10000", "OK x"]);
}

#[test]
fn codec_byte_at_a_time() {
    let mut c = LineCodec::new();
    let mut got = Vec::new();
    for b in b"OK get P 1 2 \"a\\\"b\"\nNOTIFY set P 0 0 5\n" {
        c.push(&[*b]);
        got.extend(lines(&mut c));
    }
    assert_eq!(got, ["OK get P 1 2 \"a\\\"b\"", "NOTIFY set P 0 0 5"]);
}

#[test]
fn codec_overflow_resyncs() {
    let mut c = LineCodec::new();
    c.push(&vec![b'x'; MAX_LINE + 1]);
    assert!(c.next_line().is_err());
    c.push(b"OK y\n");
    assert_eq!(lines(&mut c), ["OK y"]);
}

#[test]
fn tokenizer_quotes_and_escapes() {
    let t = tokenize(r#"OK  set  MIXER:Current/InCh/Label/Name 0 0 "A \"B\" \\C" "disp""#);
    assert_eq!(
        t,
        [
            Token::bare("OK"),
            Token::bare("set"),
            Token::bare("MIXER:Current/InCh/Label/Name"),
            Token::bare("0"),
            Token::bare("0"),
            Token::quoted(r#"A "B" \C"#),
            Token::quoted("disp"),
        ]
    );
    // Empty string, spaces inside quotes, trailing whitespace.
    assert_eq!(
        tokenize(r#"x "" "a  b"   "#),
        [Token::bare("x"), Token::quoted(""), Token::quoted("a  b")]
    );
    // Unterminated quote runs to end of line.
    assert_eq!(
        tokenize(r#"x "abc"#),
        [Token::bare("x"), Token::quoted("abc")]
    );
    assert!(tokenize("   ").is_empty());
}

#[test]
fn quote_roundtrips_through_tokenizer() {
    for s in [
        "plain",
        "",
        r#"a"b"#,
        r"back\slash",
        r#"\"both\""#,
        "  spaced  ",
    ] {
        let q = quote(s).unwrap();
        assert_eq!(tokenize(&q), [Token::quoted(s)], "{s:?} → {q}");
    }
    assert!(quote("line\nbreak").is_err());
    assert!(quote("cr\r").is_err());
}

#[test]
fn encoding_is_exact() {
    let cases = [
        (
            Command::get("MIXER:Current/InCh/Fader/Level", 0, 0),
            "get MIXER:Current/InCh/Fader/Level 0 0\n",
        ),
        (
            Command::get("MIXER:Current/InCh/ToMix/Level", 31, 19),
            "get MIXER:Current/InCh/ToMix/Level 31 19\n",
        ),
        (
            Command::set(
                "MIXER:Current/InCh/Fader/Level",
                0,
                0,
                RcpValue::Int(-32768),
            ),
            "set MIXER:Current/InCh/Fader/Level 0 0 -32768\n",
        ),
        (
            Command::set(
                "MIXER:Current/InCh/Label/Name",
                3,
                0,
                RcpValue::Str(r#"VOX "1" \ L"#.to_owned()),
            ),
            "set MIXER:Current/InCh/Label/Name 3 0 \"VOX \\\"1\\\" \\\\ L\"\n",
        ),
        (
            Command::set(
                "MIXER:Current/DCA/Label/Color",
                0,
                0,
                RcpValue::Str("SkyBlue".to_owned()),
            ),
            "set MIXER:Current/DCA/Label/Color 0 0 \"SkyBlue\"\n",
        ),
        (Command::devinfo("productname"), "devinfo productname\n"),
        (Command::devstatus("runmode"), "devstatus runmode\n"),
        (Command::prminfo(107), "prminfo 107\n"),
        (Command::sscurrent(SceneBank::B), "sscurrent_ex scene_b\n"),
        (Command::ssinfo(SceneBank::B, 22), "ssinfo_ex scene_b 22\n"),
        (
            Command::ssrecall(SceneBank::A, 5),
            "ssrecall_ex scene_a 5\n",
        ),
        (
            Command::scpmode_keepalive(10000),
            "scpmode keepalive 10000\n",
        ),
    ];
    for (cmd, wire) in cases {
        assert_eq!(cmd.encode().unwrap(), wire);
    }
    assert!(Command::get("bad address", 0, 0).encode().is_err());
    assert!(
        Command::set("A", 0, 0, RcpValue::Str("x\ny".to_owned()))
            .encode()
            .is_err()
    );
}

#[test]
fn write_classification_and_keys() {
    assert!(Command::set("A", 0, 0, RcpValue::Int(1)).is_write());
    assert!(Command::ssrecall(SceneBank::A, 1).is_write());
    assert!(!Command::get("A", 0, 0).is_write());
    assert!(!Command::scpmode_keepalive(1000).is_write());
    assert_eq!(
        Command::set("A/B", 1, 2, RcpValue::Int(1)).match_key(),
        ["A/B", "1", "2"]
    );
    assert_eq!(Command::devinfo("version").match_key(), ["version"]);
}

#[test]
fn parse_reply_kinds() {
    // Live TF1 shapes.
    let Line::Ok {
        verb,
        args,
        modified,
    } = Line::parse("OK get MIXER:Current/InCh/Label/Name 0 0 \"SYSTEM\"")
    else {
        panic!()
    };
    assert_eq!((verb.as_str(), modified), ("get", false));
    let p = ParamReply::from_args(&args).unwrap();
    assert_eq!(p.address, "MIXER:Current/InCh/Label/Name");
    assert_eq!((p.x, p.y), (0, 0));
    assert_eq!(p.value, RcpValue::Str("SYSTEM".to_owned()));
    assert_eq!(p.display, None);

    // Value is token 5, not the last token (display string follows).
    let Line::Notify { verb, args } =
        Line::parse("NOTIFY set MIXER:Current/InCh/Fader/Level 3 0 -4760 \"-47.60\"")
    else {
        panic!()
    };
    assert_eq!(verb, "set");
    let p = ParamReply::from_args(&args).unwrap();
    assert_eq!((p.x, p.value.as_int()), (3, Some(-4760)));
    assert_eq!(p.display.as_deref(), Some("-47.60"));

    let Line::Ok { modified, args, .. } =
        Line::parse("OKm set MIXER:Current/MuteMaster/On 0 0 1 \"ON\"")
    else {
        panic!()
    };
    assert!(modified);
    assert_eq!(
        ParamReply::from_args(&args).unwrap().value,
        RcpValue::Int(1)
    );

    assert_eq!(
        Line::parse("ERROR get InvalidArgument"),
        Line::Error {
            verb: "get".to_owned(),
            reason: "InvalidArgument".to_owned()
        }
    );
    assert_eq!(
        Line::parse("ERROR prminfo InternalError"),
        Line::Error {
            verb: "prminfo".to_owned(),
            reason: "InternalError".to_owned()
        }
    );
    assert_eq!(Line::parse(""), Line::Empty);
    assert!(matches!(Line::parse("garbage here"), Line::Unknown(_)));
    assert!(ParamReply::from_args(&tokenize("A 0")).is_err());
}

#[test]
fn parse_scene_replies() {
    let Line::Ok { verb, args, .. } = Line::parse("OK sscurrent_ex scene_b 22 modified") else {
        panic!()
    };
    assert_eq!(verb, "sscurrent_ex");
    let s = SceneCurrent::from_args(&args).unwrap();
    assert_eq!((s.bank, s.number, s.modified), (SceneBank::B, 22, true));
    assert_eq!(s.label(), "B22");

    let Line::Notify { args, .. } = Line::parse("NOTIFY sscurrent_ex scene_a 5") else {
        panic!()
    };
    let s = SceneCurrent::from_args(&args).unwrap();
    assert_eq!((s.bank, s.number, s.modified), (SceneBank::A, 5, false));
    assert_eq!(s.label(), "A05");

    let Line::Ok { args, .. } = Line::parse(r#"OK ssinfo_ex scene_b 22 "B22" "dave" "" user"#)
    else {
        panic!()
    };
    let i = SceneInfo::from_args(&args).unwrap();
    assert_eq!(
        (i.bank, i.number, i.title.as_str()),
        (SceneBank::B, 22, "dave")
    );
    assert_eq!(i.comment, "");
}

#[test]
fn parses_every_prminfo_line_of_the_live_dump() {
    let lines: Vec<String> = serde_json::from_str(PRMINFO).unwrap();
    assert_eq!(lines.len(), 108);
    for (i, line) in lines.iter().enumerate() {
        let row = PrmInfo::parse_line(line).unwrap_or_else(|e| panic!("{line}: {e}"));
        assert_eq!(row.index, u32::try_from(i).unwrap());
        assert!(row.address.starts_with("MIXER:"), "{line}");
        assert!(row.readable, "{line}");
        assert_eq!(row.ui, "any");
    }
    let first = PrmInfo::parse_line(&lines[0]).unwrap();
    assert_eq!(first.address, "MIXER:Current/InCh/Fader/Level");
    assert_eq!((first.x_count, first.y_count), (40, 0));
    assert_eq!((first.min, first.max), (-32768, 1000));
    assert_eq!(first.default, RcpValue::Int(-32768));
    assert_eq!(first.unit, "dB");
    assert_eq!(first.ty, PrmType::Integer);
    assert!(first.writable);
    assert_eq!(first.scale, 100);

    let tomix = PrmInfo::parse_line(&lines[10]).unwrap();
    assert_eq!(tomix.address, "MIXER:Current/InCh/ToMix/Level");
    assert_eq!(tomix.y_count, 20);

    let mute_name = PrmInfo::parse_line(&lines[107]).unwrap();
    assert_eq!(mute_name.address, "MIXER:Current/MuteMaster/Label/Name");
    assert_eq!((mute_name.max, mute_name.ty.clone()), (8, PrmType::Binary));
    assert!(!mute_name.writable, "read-only on TF1 V4.55");
}

#[test]
fn embedded_asset_matches_fixture() {
    assert_eq!(TF1_PRMINFO_JSON, PRMINFO);
}
