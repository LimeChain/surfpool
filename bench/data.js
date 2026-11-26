window.BENCHMARK_DATA = {
  "lastUpdate": 1795686414819,
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
      },
      {
        "commit": {
          "author": {
            "email": "aryansputta@gmail.com",
            "name": "Aryan Putta",
            "username": "aryanputta"
          },
          "committer": {
            "email": "noreply@github.com",
            "name": "GitHub",
            "username": "web-flow"
          },
          "distinct": true,
          "id": "e9fb8ff46d4aa2b98bf599abfba56aae490fd17c",
          "message": "fix(mcp): reject overflowing token balance amounts (#919)",
          "timestamp": "2026-11-25T09:41:02-05:00",
          "tree_id": "e2ecc1b116c9cee9f61de6a6b127ad1b6cb1f7f5",
          "url": "https://github.com/LimeChain/surfpool/commit/e9fb8ff46d4aa2b98bf599abfba56aae490fd17c"
        },
        "date": 1795618683892,
        "tool": "customBiggerIsBetter",
        "benches": [
          {
            "name": "gpa/rps",
            "value": 106.1,
            "range": "105.3..109.5",
            "unit": "req/s"
          },
          {
            "name": "reads/rps",
            "value": 8464.5,
            "range": "8439.7..8481.2",
            "unit": "req/s"
          },
          {
            "name": "send_tx_default/send_rate",
            "value": 1662.3,
            "range": "1634.2..1664.8",
            "unit": "tx/s"
          },
          {
            "name": "send_tx_default/tps_confirmed",
            "value": 1626.4036682215356,
            "range": "1600.5..1629.0",
            "unit": "tx/s"
          },
          {
            "name": "send_tx_no_profiling/send_rate",
            "value": 1682.5,
            "range": "1679.0..1701.3",
            "unit": "tx/s"
          },
          {
            "name": "send_tx_no_profiling/tps_confirmed",
            "value": 1642.0170622577527,
            "range": "1633.0..1666.6",
            "unit": "tx/s"
          }
        ]
      },
      {
        "commit": {
          "author": {
            "email": "41898282+github-actions[bot]@users.noreply.github.com",
            "name": "github-actions[bot]",
            "username": "github-actions[bot]"
          },
          "committer": {
            "email": "noreply@github.com",
            "name": "GitHub",
            "username": "web-flow"
          },
          "distinct": false,
          "id": "927ddbebe5892fd3053692fd179b6eb36fa218fb",
          "message": "chore(release): publish v1.8.0 (#976)\n\nCo-authored-by: github-actions[bot] <41898282+github-actions[bot]@users.noreply.github.com>",
          "timestamp": "2026-11-25T16:33:11-05:00",
          "tree_id": "f00ac4e7bdd23bb11d60157d8a63b9834930b297",
          "url": "https://github.com/LimeChain/surfpool/commit/927ddbebe5892fd3053692fd179b6eb36fa218fb"
        },
        "date": 1795686414169,
        "tool": "customBiggerIsBetter",
        "benches": [
          {
            "name": "gpa/rps",
            "value": 129,
            "range": "119.4..131.4",
            "unit": "req/s"
          },
          {
            "name": "reads/rps",
            "value": 11500,
            "range": "11095.2..11595.2",
            "unit": "req/s"
          },
          {
            "name": "send_tx_default/send_rate",
            "value": 1990.8,
            "range": "1952.5..1997.9",
            "unit": "tx/s"
          },
          {
            "name": "send_tx_default/tps_confirmed",
            "value": 1942.2126726066697,
            "range": "1906.2..1949.9",
            "unit": "tx/s"
          },
          {
            "name": "send_tx_no_profiling/send_rate",
            "value": 2015.6,
            "range": "2013.5..2035.0",
            "unit": "tx/s"
          },
          {
            "name": "send_tx_no_profiling/tps_confirmed",
            "value": 1965.774004389058,
            "range": "1964.6..1991.3",
            "unit": "tx/s"
          }
        ]
      }
    ]
  }
}