#!/usr/bin/env python3

from __future__ import annotations

import pathlib
import sys
import unittest
from unittest import mock

sys.path.insert(0, str(pathlib.Path(__file__).parent))

import aggregate
import measure
import netem_diagnose


class AggregateMethodologyTests(unittest.TestCase):
    def test_netem_diagnosis_artifact_requires_transport_observations(self) -> None:
        sample = {
            "range": {"ok": True, "latency_ms": 600.0},
            "qdisc_delta": {"bytes": 1, "packets": 1, "dropped": 0, "overlimits": 0, "requeues": 0},
            "tcp": {"samples": [{"observed_at_ns": 1, "rtt_ms": 80.0, "cwnd": 10}]},
        }
        result = {
            "quiescence": {"ok": True},
            "samples": [
                {
                    "range": dict(sample["range"]),
                    "qdisc_delta": dict(sample["qdisc_delta"]),
                    "tcp": {"ready": True, "samples": list(sample["tcp"]["samples"])},
                }
                for _ in range(netem_diagnose.SAMPLES)
            ]
        }

        self.assertEqual(netem_diagnose.artifact_errors(result), [])
        result["samples"][3]["tcp"] = {"samples": []}
        self.assertEqual(netem_diagnose.artifact_errors(result), ["sample 3: missing TCP observation"])

    def test_netem_diagnosis_parses_qdisc_and_tcp_counters(self) -> None:
        qdisc = {
            "ok": True,
            "stdout": "qdisc netem 8001: root\n Sent 274523 bytes 185 pkt (dropped 1, overlimits 0 requeues 0)\n",
        }
        tcp = "123\nESTAB 0 0 127.0.0.1:8090 127.0.0.1:40123\n cubic rtt:80.125/1.500 cwnd:10 bytes_retrans:1448 retrans:1/3\n\x1e\n"

        self.assertEqual(
            netem_diagnose.qdisc_counters(qdisc),
            {"bytes": 274523, "packets": 185, "dropped": 1, "overlimits": 0, "requeues": 0},
        )
        self.assertEqual(
            netem_diagnose.parse_tcp_samples(tcp),
            [{"observed_at_ns": 123, "rtt_ms": 80.125, "rtt_variance_ms": 1.5, "cwnd": 10, "bytes_retrans": 1448, "retrans": 1, "retrans_total": 3}],
        )

        link = {
            "ok": True,
            "stdout": """11: eth0@if1899: <BROADCAST>\n    RX:  bytes packets errors dropped  missed   mcast\n       8862868    7316      0       0       0       0\n    TX:  bytes packets errors dropped carrier collsns\n       5999514    3310      0       0       0       0""",
        }
        self.assertEqual(
            netem_diagnose.link_counters(link),
            {"rx_bytes": 8862868, "rx_packets": 7316, "rx_errors": 0, "rx_dropped": 0, "tx_bytes": 5999514, "tx_packets": 3310, "tx_errors": 0, "tx_dropped": 0},
        )

    @mock.patch.object(measure, "post_json")
    def test_netem_diagnosis_waits_for_the_api(self, post_json: mock.Mock) -> None:
        post_json.side_effect = [
            {"ok": False, "error": "connection reset"},
            {"ok": True, "status": 200, "json": []},
        ]

        readiness = netem_diagnose.wait_for_api("http://example.test", timeout_s=1)

        self.assertTrue(readiness["ok"])
        self.assertEqual(post_json.call_count, 2)

    def test_netem_p95_is_kept_per_run(self) -> None:
        runs = [
            {"events": [{"scenario": "netem-delay-loss", "ranges": [{"latency_ms": value} for value in values]}]}
            for values in (range(1, 21), range(21, 41))
        ]

        summaries = aggregate.metric_summaries_by_run(runs, "netem-delay-loss", "latency_ms")

        self.assertEqual([summary["count"] for summary in summaries], [20, 20])
        self.assertEqual([summary["p95"] for summary in summaries], [19.0, 39.0])
        self.assertTrue(aggregate.every_run_within(summaries, 39.0))
        self.assertFalse(aggregate.every_run_within(summaries, 38.0))
        self.assertEqual(aggregate.netem_sample_counts(runs), [20, 20])

    @mock.patch.object(measure, "clear_netem", return_value={"ok": True})
    @mock.patch.object(measure, "apply_netem", return_value={"ok": True})
    @mock.patch.object(measure, "range_request")
    def test_netem_scenario_collects_twenty_validated_ranges(
        self,
        request: mock.Mock,
        _apply: mock.Mock,
        _clear: mock.Mock,
    ) -> None:
        request.side_effect = [
            {"ok": True, "latency_ms": float(index), "stall": False}
            for index in range(20)
        ]
        scenario = {
            "id": "netem-delay-loss",
            "samples": 20,
            "network": {"delay_ms": 80, "loss_percent": 1},
        }

        events = measure.run_scenario(
            "http://example.test",
            scenario,
            "file:///fixture.torrent",
            "00" * 20,
            "target-container",
            None,
            None,
            "tooling-image",
        )

        self.assertEqual(request.call_count, 20)
        self.assertEqual(events[0]["sample_count"], 20)
        self.assertEqual(len(events[0]["ranges"]), 20)

    @mock.patch.object(measure, "range_request", return_value={"ok": True, "latency_ms": 1.0, "stall": False})
    @mock.patch.object(measure, "prepare", return_value=[])
    def test_reference_seek_evicted_does_not_require_rustorr_control(
        self,
        _prepare: mock.Mock,
        _request: mock.Mock,
    ) -> None:
        events = measure.run_scenario(
            "http://example.test",
            {"id": "seek-evicted", "state": "evicted"},
            "file:///fixture.torrent",
            "00" * 20,
            None,
            None,
            None,
            "tooling-image",
        )

        self.assertFalse(any(event.get("scenario") == "eviction-control" for event in events))
        self.assertEqual(events[-1]["scenario"], "seek-evicted")


if __name__ == "__main__":
    unittest.main()
