window.BENCHMARK_DATA = {
  "lastUpdate": 1795599099542,
  "repoUrl": "https://github.com/LimeChain/surfpool",
  "entries": {
    "surfpool throughput": [
      {
        "commit": {
          "author": {
            "email": "micaiahreid@gmail.com",
            "name": "Micaiah Reid",
            "username": "MicaiahReid"
          },
          "committer": {
            "email": "noreply@github.com",
            "name": "GitHub",
            "username": "web-flow"
          },
          "distinct": false,
          "id": "1fab002b559c5b5345cf92766b50ba1418d9f1c1",
          "message": "ci: add RPC throughput regression benchmarks (#951)",
          "timestamp": "2026-11-23T15:53:13-05:00",
          "tree_id": "80fa374594f57bbdce2b448163efa2c066165f95",
          "url": "https://github.com/LimeChain/surfpool/commit/1fab002b559c5b5345cf92766b50ba1418d9f1c1"
        },
        "date": 1795509991745,
        "tool": "customBiggerIsBetter",
        "benches": [
          {
            "name": "gpa/rps",
            "value": 104.1,
            "range": "103.7..104.5",
            "unit": "req/s"
          },
          {
            "name": "reads/rps",
            "value": 8355.4,
            "range": "8289.9..8419.7",
            "unit": "req/s"
          },
          {
            "name": "send_tx_default/send_rate",
            "value": 1656.7,
            "range": "1602.3..1659.8",
            "unit": "tx/s"
          },
          {
            "name": "send_tx_default/tps_confirmed",
            "value": 1622.2733052054894,
            "range": "1562.7..1623.8",
            "unit": "tx/s"
          },
          {
            "name": "send_tx_no_profiling/send_rate",
            "value": 1691.4,
            "range": "1675.2..1693.3",
            "unit": "tx/s"
          },
          {
            "name": "send_tx_no_profiling/tps_confirmed",
            "value": 1650.993053492507,
            "range": "1626.5..1653.3",
            "unit": "tx/s"
          }
        ]
      },
      {
        "commit": {
          "author": {
            "email": "bakasura@abv.bg",
            "name": "bakasura980",
            "username": "bakasura980"
          },
          "committer": {
            "email": "noreply@github.com",
            "name": "GitHub",
            "username": "web-flow"
          },
          "distinct": false,
          "id": "309014210db043b30b31f859b468ae4ef3fd60d8",
          "message": "feat(scenarios): add GoonFi support - state templates & presets (#965)\n\nCo-authored-by: Georgi Petroff <102983346+92Infinitus92@users.noreply.github.com>",
          "timestamp": "2026-11-24T13:46:28-05:00",
          "tree_id": "bcc8f259ff6d156b5c42f9bad82e2bad7fdf3122",
          "url": "https://github.com/LimeChain/surfpool/commit/309014210db043b30b31f859b468ae4ef3fd60d8"
        },
        "date": 1795599098953,
        "tool": "customBiggerIsBetter",
        "benches": [
          {
            "name": "gpa/rps",
            "value": 110.1,
            "range": "107.2..110.3",
            "unit": "req/s"
          },
          {
            "name": "reads/rps",
            "value": 9416.5,
            "range": "9358.8..9476.6",
            "unit": "req/s"
          },
          {
            "name": "send_tx_default/send_rate",
            "value": 1726,
            "range": "1724.5..1739.5",
            "unit": "tx/s"
          },
          {
            "name": "send_tx_default/tps_confirmed",
            "value": 1687.7043322201232,
            "range": "1687.5..1702.0",
            "unit": "tx/s"
          },
          {
            "name": "send_tx_no_profiling/send_rate",
            "value": 1776.6,
            "range": "1766.6..1778.2",
            "unit": "tx/s"
          },
          {
            "name": "send_tx_no_profiling/tps_confirmed",
            "value": 1727.4416597721129,
            "range": "1719.7..1731.2",
            "unit": "tx/s"
          }
        ]
      }
    ]
  }
}