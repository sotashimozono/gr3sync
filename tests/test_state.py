from __future__ import annotations

from gr3sync.state import LEDGER_FILENAME, Ledger, already_have


def test_ledger_round_trips(tmp_path):
    ledger = Ledger.load(tmp_path)
    ledger.record("100RICOH/a.JPG", size=1234, camera="RICOH GR III")
    ledger.save()

    reloaded = Ledger.load(tmp_path)
    assert "100RICOH/a.JPG" in reloaded
    assert reloaded.downloaded["100RICOH/a.JPG"]["size"] == 1234


def test_a_corrupt_ledger_does_not_block_a_sync(tmp_path):
    (tmp_path / LEDGER_FILENAME).write_text("{not json", encoding="utf-8")
    ledger = Ledger.load(tmp_path)
    assert ledger.downloaded == {}
    ledger.record("100RICOH/a.JPG", size=1)
    ledger.save()
    assert "100RICOH/a.JPG" in Ledger.load(tmp_path)


def test_file_on_disk_counts_even_without_a_ledger(tmp_path):
    (tmp_path / "100RICOH").mkdir()
    (tmp_path / "100RICOH" / "a.JPG").write_bytes(b"x")
    assert already_have(tmp_path, "100RICOH/a.JPG", None)


def test_ledger_covers_a_file_moved_out_of_the_inbox(tmp_path):
    """Importing into a photo manager must not cause a full re-download."""
    ledger = Ledger.load(tmp_path)
    ledger.record("100RICOH/a.JPG", size=10)
    assert not (tmp_path / "100RICOH" / "a.JPG").exists()
    assert already_have(tmp_path, "100RICOH/a.JPG", ledger)


def test_unknown_key_is_not_claimed(tmp_path):
    assert not already_have(tmp_path, "100RICOH/missing.JPG", Ledger.load(tmp_path))


def test_save_is_atomic_and_leaves_no_temp_files(tmp_path):
    ledger = Ledger.load(tmp_path)
    for index in range(5):
        ledger.record(f"100RICOH/{index}.JPG", size=index)
        ledger.save()
    assert sorted(p.name for p in tmp_path.iterdir()) == [LEDGER_FILENAME]
