"""Tests for the baseline harness's own logic: statistics, the procfs walk,
the report table, the simulators and the data-copy guards. They start no
pp-server.

python3 -B -m unittest discover -s scripts/perf-baseline -p 'test_*.py' -v
"""

import json
import struct
import tempfile
import unittest
import urllib.request
import zlib
from unittest import mock
from pathlib import Path

import baseline
import perfstats
import runtime
import scenarios


class Statistics(unittest.TestCase):
    def test_median_of_odd_and_even_counts(self):
        self.assertEqual(perfstats.median([3, 1, 2]), 2)
        self.assertEqual(perfstats.median([4, 1, 3, 2]), 2.5)

    def test_nearest_rank_p95(self):
        samples = list(range(1, 21))  # 1..20
        self.assertEqual(perfstats.percentile(samples, 95), 19)
        self.assertEqual(perfstats.percentile(samples, 100), 20)
        self.assertEqual(perfstats.percentile([7], 95), 7)
        # Five samples: ceil(0.95 * 5) = 5, the maximum.
        self.assertEqual(perfstats.percentile([5, 1, 4, 2, 3], 95), 5)

    def test_empty_and_bad_percentiles_are_refused(self):
        with self.assertRaises(ValueError):
            perfstats.median([])
        with self.assertRaises(ValueError):
            perfstats.percentile([], 50)
        with self.assertRaises(ValueError):
            perfstats.percentile([1], 0)

    def test_summary_reports_spread_from_median(self):
        summary = perfstats.summarize([10, 10, 10, 10, 20])
        self.assertEqual(summary["median"], 10)
        self.assertEqual(summary["p95"], 20)
        self.assertEqual(summary["spread_pct"], 100.0)
        self.assertEqual((summary["min"], summary["max"], summary["n"]), (10, 20, 5))
        self.assertEqual(perfstats.summarize([0, 0])["spread_pct"], 0.0)


def fake_proc(root, tree):
    """tree: {pid: (pss_kib or None, [child pids])}; a None PSS has no
    smaps_rollup, as for a process that exited."""
    for pid, (pss, children) in tree.items():
        task = root / str(pid) / "task" / str(pid)
        task.mkdir(parents=True)
        (task / "children").write_text(" ".join(map(str, children)) + (" " if children else ""))
        if pss is not None:
            (root / str(pid) / "smaps_rollup").write_text(
                f"00400000-7fff0000 ---p 00000000 00:00 0 [rollup]\nRss: {pss * 2} kB\nPss: {pss} kB\n")


class ProcfsTree(unittest.TestCase):
    def test_sums_pss_over_descendants(self):
        with tempfile.TemporaryDirectory() as temp:
            proc = Path(temp)
            fake_proc(proc, {10: (100, [11, 12]), 11: (50, [13]), 12: (25, []), 13: (5, [])})
            self.assertEqual(perfstats.tree_pids(10, proc), [10, 11, 12, 13])
            self.assertEqual(perfstats.tree_pss_bytes(10, proc), (180 * 1024, 4))

    def test_exited_processes_are_left_out(self):
        with tempfile.TemporaryDirectory() as temp:
            proc = Path(temp)
            fake_proc(proc, {10: (100, [11, 99]), 11: (None, [])})
            # 99 has no directory at all; 11 lost its smaps_rollup.
            self.assertEqual(perfstats.tree_pss_bytes(10, proc), (100 * 1024, 1))

    def test_threads_children_files_are_all_read(self):
        with tempfile.TemporaryDirectory() as temp:
            proc = Path(temp)
            fake_proc(proc, {10: (1, []), 20: (2, [])})
            extra = proc / "10" / "task" / "15"
            extra.mkdir(parents=True)
            (extra / "children").write_text("20 ")
            self.assertEqual(perfstats.tree_pids(10, proc), [10, 20])


class CpuTicks(unittest.TestCase):
    def test_stat_fields_after_the_command_name(self):
        line = "42 (a) b (c)) S 1 42 42 0 -1 4194560 100 0 0 0 7 3 2 1 20 0 1 0 5 0 0\n"
        self.assertEqual(perfstats.stat_ticks(line), 7 + 3 + 2 + 1)

    def test_this_process_has_parseable_ticks(self):
        ticks = perfstats.stat_ticks(Path("/proc/self/stat").read_text())
        self.assertGreaterEqual(ticks, 0)

    def test_delta_counts_new_processes_and_never_goes_negative(self):
        self.assertEqual(perfstats.cpu_delta({1: 10, 2: 5}, {1: 15, 3: 4}), 5 + 4)
        self.assertEqual(perfstats.cpu_delta({1: 10}, {1: 8}), 0)

    def test_tree_ticks_over_fake_proc(self):
        with tempfile.TemporaryDirectory() as temp:
            proc = Path(temp)
            fake_proc(proc, {10: (1, [11]), 11: (1, [])})
            for pid, utime in ((10, 4), (11, 6)):
                (proc / str(pid) / "stat").write_text(
                    f"{pid} (x) S 1 1 1 0 -1 0 0 0 0 0 {utime} 1 0 0 20 0 1 0 5 0 0\n")
            self.assertEqual(perfstats.tree_cpu_ticks(10, proc), {10: 5, 11: 7})


class RunControl(unittest.TestCase):
    def test_rounds_alternate_which_runtime_goes_first(self):
        self.assertEqual(baseline.run_order(1), ("node", "rust"))
        self.assertEqual(baseline.run_order(2), ("rust", "node"))
        self.assertEqual(baseline.run_order(3), ("node", "rust"))

    def test_wait_for_quiet_returns_at_once_when_quiet_and_gives_up_at_timeout(self):
        loads = iter([(1.0, 0, 0), (1.0, 0, 0)])
        with mock.patch.object(baseline.os, "getloadavg", lambda: next(loads)):
            self.assertEqual(baseline.wait_for_quiet(2.0, 60), 1.0)
        with mock.patch.object(baseline.os, "getloadavg", lambda: (9.0, 0, 0)), \
                mock.patch.object(baseline.time, "sleep") as sleep:
            self.assertEqual(baseline.wait_for_quiet(2.0, 0), 9.0)
            sleep.assert_not_called()


class Report(unittest.TestCase):
    def test_table_marks_missing_sides(self):
        node = perfstats.summarize([100, 110, 120])
        rust = perfstats.summarize([50, 55, 60])
        text = perfstats.table([
            {"measure": "Builds list", "unit": "ms", "node": node, "rust": rust},
            {"measure": "STL pack", "unit": "ms", "node": node, "rust": None,
             "note": "not available in Rust"},
            {"measure": "Idle PSS", "unit": "bytes", "node": perfstats.summarize([2 * 1024 * 1024]),
             "rust": perfstats.summarize([1024 * 1024])},
        ])
        lines = text.splitlines()
        self.assertIn("| Builds list | 110 ms | 120 ms (max) | 55.0 ms | 60.0 ms (max) | 0.50x | 3/3 |", lines[2])
        self.assertIn("| - | - | - | 3/0 | not available in Rust |", lines[3])
        self.assertIn("2.0 MiB", lines[4])
        self.assertIn("0.50x", lines[4])

    def test_p95_is_not_marked_with_enough_samples_and_ratio_can_be_dropped(self):
        many = perfstats.summarize(list(range(1, 21)))
        text = perfstats.table([{"measure": "Burst", "unit": "ms", "node": many, "rust": many,
                                 "comparable": False, "note": "different"}])
        self.assertIn("| Burst | 10.5 ms | 19.0 ms | 10.5 ms | 19.0 ms | - | 20/20 | different |",
                      text.splitlines()[2])

    def test_report_rows_follow_measures_with_notes_and_comparability(self):
        samples = {
            "node": {"builds_list_warm_ms": [10, 20], "export_stl_pack_ms": [100],
                     "checkoff_burst_ms": [5], "plan_node_publishing_save_ms": [3]},
            "rust": {"builds_list_warm_ms": [5, 5], "checkoff_burst_ms": [4],
                     "plan_autosave_ms": [2]},
        }
        rows = {row["measure"]: row for row in baseline.report(samples)}
        self.assertEqual(rows["Builds list, warm"]["note"], "")
        self.assertTrue(rows["Builds list, warm"]["comparable"])
        self.assertEqual(rows["STL pack export, first in run"]["note"], "not available in Rust")
        self.assertIsNone(rows["STL pack export, first in run"]["rust"])
        self.assertFalse(rows["Checkoff burst, write + screen refresh"]["comparable"])
        self.assertEqual(rows["Checkoff burst, write + screen refresh"]["note"], baseline.DIFFERENT)
        self.assertEqual(rows["Plan change: autosave"]["note"], "Node has no draft-only autosave")
        self.assertIn("Node only", rows["Plan change: Node one-request Save"]["note"])
        # Measures with no samples on either side are left out.
        self.assertNotIn("Idle CPU, process tree (% of one core)", rows)
        order = [row["label"] for row in baseline.MEASURES if row["label"] in rows]
        self.assertEqual(order, list(rows))

    def test_results_json_round_trips_with_one_line_per_measure(self):
        meta = {"complete": True, "machine": {"cpus": 12}}
        samples = {"node": {"b": [1.23456, 2], "a": [3]}, "rust": {}}
        text = baseline.dump_results(meta, samples)
        self.assertEqual(json.loads(text), {"metadata": meta, "samples": {
            "node": {"a": [3], "b": [1.235, 2]}, "rust": {}}})
        self.assertIn('   "b": [1.235, 2]\n', text)

    def test_measure_keys_are_unique(self):
        keys = [row["key"] for row in baseline.MEASURES]
        self.assertEqual(len(keys), len(set(keys)))

    def test_ratio_needs_both_sides(self):
        self.assertEqual(perfstats.ratio(None, 1), "-")
        self.assertEqual(perfstats.ratio(0, 1), "-")
        self.assertEqual(perfstats.ratio(2, None), "-")
        self.assertEqual(perfstats.ratio(4, 1), "0.25x")


class Simulator(unittest.TestCase):
    def test_answers_status_then_complete_and_counts(self):
        printer = runtime.SimulatedPrinter()
        try:
            self.assertTrue(printer.url.startswith("http://127.0.0.1:"))
            with urllib.request.urlopen(printer.url + "/printer/objects/query?print_stats") as reply:
                state = json.load(reply)["result"]["status"]["print_stats"]["state"]
            self.assertEqual(state, "printing")
            printer.finish()
            with urllib.request.urlopen(printer.url + "/printer/objects/query") as reply:
                status = json.load(reply)["result"]["status"]
            self.assertEqual(status["print_stats"]["state"], "complete")
            self.assertEqual(status["virtual_sdcard"]["progress"], 1.0)
            with self.assertRaises(urllib.error.HTTPError) as missing:
                urllib.request.urlopen(printer.url + "/printer/print/start")
            self.assertEqual(missing.exception.code, 404)
            missing.exception.close()
            self.assertEqual(printer.requests, 3)
        finally:
            printer.close()


class ScenarioHelpers(unittest.TestCase):
    def test_bursts_cover_every_unit_once_in_order(self):
        units = list(range(100))
        groups = scenarios.bursts(units)
        self.assertEqual(len(groups), 15)
        self.assertEqual([unit for group in groups for unit in group], units)
        self.assertLessEqual(max(map(len, groups)) - min(map(len, groups)), 1)
        with self.assertRaises(ValueError):
            scenarios.bursts([1, 2], 3)
        with self.assertRaises(ValueError):
            scenarios.bursts([1], 0)

    def test_thumbnail_png_is_a_valid_256_square_png(self):
        png = scenarios.thumbnail_png()
        self.assertEqual(png[:8], b"\x89PNG\r\n\x1a\n")
        self.assertEqual(png[12:16], b"IHDR")
        self.assertEqual(struct.unpack(">II", png[16:24]), (256, 256))
        length = struct.unpack(">I", png[33:37])[0]
        self.assertEqual(png[37:41], b"IDAT")
        raw = zlib.decompress(png[41:41 + length])
        self.assertEqual(len(raw), 256 * (1 + 256 * 4))
        self.assertTrue(png.endswith(b"IEND\xaeB`\x82"))

    def test_multipart_wraps_one_file(self):
        body, content_type = scenarios.multipart("file", "thumbnail.png", b"PNG", "image/png")
        boundary = content_type.split("boundary=")[1]
        self.assertTrue(body.startswith(f"--{boundary}\r\n".encode()))
        self.assertIn(b'name="file"; filename="thumbnail.png"', body)
        self.assertTrue(body.endswith(f"\r\n\r\nPNG\r\n--{boundary}--\r\n".encode()))

    def test_renderer_version_is_read_from_the_contract(self):
        self.assertRegex(scenarios.renderer_version(), r"^\w+$")


class DataGuards(unittest.TestCase):
    def test_copy_refuses_a_directory_without_the_synthetic_manifest(self):
        with tempfile.TemporaryDirectory() as temp:
            with self.assertRaises(SystemExit):
                runtime.copy_dataset(Path(temp), Path(temp) / "copy", "node")

    def test_marker_is_valid_json_and_never_overwritten(self):
        with tempfile.TemporaryDirectory() as temp:
            runtime.mark_rust_owned(temp)
            marker = json.loads((Path(temp) / runtime.MARKER).read_text())
            self.assertEqual((marker["version"], marker["owner"]), (1, "rust"))
            self.assertRegex(marker["created_at"], r"^\d{4}-\d\d-\d\dT\d\d:\d\d:\d\dZ$")
            with self.assertRaises(FileExistsError):
                runtime.mark_rust_owned(temp)


if __name__ == "__main__":
    unittest.main()
