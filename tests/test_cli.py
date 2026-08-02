from __future__ import annotations

import json

import pytest

from gr3sync.cli import Reporter, build_parser, main


@pytest.fixture(autouse=True)
def isolated_config(monkeypatch, tmp_path):
    """Keep the developer's real ~/.config/gr3sync out of the tests."""
    monkeypatch.setenv("XDG_CONFIG_HOME", str(tmp_path / "config"))
    monkeypatch.setenv("HOME", str(tmp_path / "home"))


# -- parser -----------------------------------------------------------------


def test_a_subcommand_is_required(capsys):
    with pytest.raises(SystemExit):
        build_parser().parse_args([])


def test_pull_defaults():
    args = build_parser().parse_args(["pull"])
    assert args.dest is None
    assert not args.no_ble
    assert not args.dry_run
    assert args.last is None


def test_pull_accepts_the_full_flag_set():
    args = build_parser().parse_args(
        ["pull", "/tmp/shots", "--no-ble", "--dry-run", "--flatten", "-r", "-l", "5", "-d", "101RICOH"]
    )
    assert args.dest == "/tmp/shots"
    assert args.no_ble and args.dry_run and args.flatten and args.raw
    assert (args.last, args.dir) == (5, "101RICOH")


def test_wlan_only_takes_on_or_off():
    assert build_parser().parse_args(["wlan", "on"]).state == "on"
    with pytest.raises(SystemExit):
        build_parser().parse_args(["wlan", "sideways"])


# -- format filter ----------------------------------------------------------


@pytest.mark.parametrize(
    ("argv", "expected"),
    [
        (["list"], (True, True)),
        (["list", "-j"], (True, False)),
        (["list", "-r"], (False, True)),
        (["list", "-j", "-r"], (True, True)),
    ],
)
def test_format_filter(argv, expected):
    from gr3sync.cli import _format_filter

    assert _format_filter(build_parser().parse_args(argv)) == expected


# -- commands against the fake camera ---------------------------------------


def test_list_prints_every_file(camera_server, capsys):
    assert main(["list", "--host", camera_server.host]) == 0
    out = capsys.readouterr().out
    assert "RICOH GR III" in out
    assert "100RICOH/R0000001.JPG" in out
    assert "6 files" in out


def test_list_json_is_parseable(camera_server, capsys):
    assert main(["--json", "list", "--host", camera_server.host, "-j"]) == 0
    payload = json.loads(capsys.readouterr().out)
    assert payload["model"] == "RICOH GR III"
    assert len(payload["photos"]) == 3
    assert all(item["file"].endswith(".JPG") for item in payload["photos"])


def test_list_without_a_camera_explains_what_to_do(capsys):
    assert main(["list", "--host", "127.0.0.1:1", "--timeout", "0"]) == 2
    assert "gr3sync wlan on" in capsys.readouterr().err


def test_get_downloads_named_files(camera_server, tmp_path, capsys):
    dest = tmp_path / "out"
    code = main(["get", "100RICOH/R0000001.JPG", "--dest", str(dest), "--host", camera_server.host])
    assert code == 0
    assert (dest / "100RICOH" / "R0000001.JPG").exists()


def test_get_rejects_a_bare_filename(camera_server, capsys):
    assert main(["get", "R0000001.JPG", "--host", camera_server.host]) == 2
    assert "DIR/FILE" in capsys.readouterr().err


def test_backends_reports_the_manual_fallback(capsys):
    assert main(["backends"]) == 0
    assert "manual" in capsys.readouterr().out


def test_config_show_reports_the_path(capsys):
    assert main(["--json", "config", "show"]) == 0
    payload = json.loads(capsys.readouterr().out)
    assert payload["_exists"] is False
    assert payload["host"] == "192.168.0.1"


# -- reporter ---------------------------------------------------------------


def test_json_reporter_emits_one_object_per_line(capsys):
    reporter = Reporter(as_json=True, verbose=False)
    reporter({"event": "plan", "pending": 2, "skipped": 0})
    reporter({"event": "download.done", "photo": "100RICOH/a.JPG", "bytes": 10})
    lines = capsys.readouterr().out.strip().splitlines()
    assert [json.loads(line)["event"] for line in lines] == ["plan", "download.done"]


def test_human_reporter_stays_quiet_about_noise_unless_verbose(capsys):
    Reporter(as_json=False, verbose=False)({"event": "ble.disconnected"})
    assert capsys.readouterr().out == ""

    Reporter(as_json=False, verbose=True)({"event": "ble.disconnected"})
    assert "ble.disconnected" in capsys.readouterr().out


def test_human_reporter_summary_flags_failures(capsys):
    Reporter(as_json=False, verbose=False)(
        {
            "event": "done",
            "dry_run": False,
            "bytes_written": 5 * 1024 * 1024,
            "downloaded": ["a", "b"],
            "skipped": ["c"],
            "failed": [{"photo": "d", "error": "boom"}],
        }
    )
    out = capsys.readouterr().out
    assert "downloaded 2 files (5.0 MiB)" in out
    assert "1 FAILED" in out
