# Initial Benchmark Run

```
running 13 tests
test backend::memory::tests::entry_invalidated_after_stale_window ... ignored
test backend::memory::tests::set_and_get_returns_cached_entry ... ignored
test layer::tests::cache_control_disallows_detects_no_cache_directives ... ignored
test layer::tests::classify_hit_marks_entry_expired_after_stale_window ... ignored
test layer::tests::classify_hit_marks_entry_fresh_when_not_near_expiry ... ignored
test layer::tests::classify_hit_marks_entry_stale_when_within_refresh_window ... ignored
test layer::tests::classify_hit_marks_entry_stale_when_within_stale_window ... ignored
test layer::tests::stampede_guard_drop_removes_lock_entry ... ignored
test policy::tests::headers_to_cache_respects_allowlist ... ignored
test policy::tests::method_predicate_overrides_default_behavior ... ignored
test policy::tests::ttl_for_prefers_primary_ttl_for_cacheable_status ... ignored
test policy::tests::ttl_for_uses_negative_ttl_for_client_error ... ignored
test policy::tests::with_statuses_updates_allowlist ... ignored

test result: ok. 0 passed; 0 failed; 13 ignored; 0 measured; 0 filtered out; finished in 0.00s

layer_throughput/baseline_inner
                        time:   [1.4057 ms 1.4149 ms 1.4231 ms]
Found 3 outliers among 50 measurements (6.00%)
  1 (2.00%) low severe
  1 (2.00%) low mild
  1 (2.00%) high severe

layer_throughput/cache_hit
                        time:   [662.06 ns 666.34 ns 671.88 ns]
Found 7 outliers among 50 measurements (14.00%)
  3 (6.00%) high mild
  4 (8.00%) high severe

layer_throughput/cache_miss
                        time:   [676.36 ns 678.49 ns 681.11 ns]
Found 6 outliers among 50 measurements (12.00%)
  1 (2.00%) low severe
  4 (8.00%) high mild
  1 (2.00%) high severe

layer_throughput/backend_hit_count
                        time:   [323.03 ps 324.34 ps 325.83 ps]
Found 4 outliers among 50 measurements (8.00%)
  1 (2.00%) low mild
  2 (4.00%) high mild
  1 (2.00%) high severe

key_extractor/path      time:   [23.665 ns 23.817 ns 24.085 ns]
Found 5 outliers among 50 measurements (10.00%)
  4 (8.00%) high mild
  1 (2.00%) high severe

key_extractor/path_and_query
                        time:   [96.706 ns 97.407 ns 98.589 ns]
Found 2 outliers among 50 measurements (4.00%)
  1 (2.00%) high mild
  1 (2.00%) high severe

key_extractor/custom_hit
                        time:   [84.486 ns 84.734 ns 85.029 ns]
Found 4 outliers among 50 measurements (8.00%)
  3 (6.00%) high mild
  1 (2.00%) high severe

key_extractor/custom_miss
                        time:   [1.3434 ns 1.3508 ns 1.3627 ns]
Found 7 outliers among 50 measurements (14.00%)
  2 (4.00%) high mild
  5 (10.00%) high severe

backend/in_memory/get_small_hit
                        time:   [308.05 ns 308.83 ns 309.79 ns]
Found 5 outliers among 50 measurements (10.00%)
  1 (2.00%) high mild
  4 (8.00%) high severe

backend/in_memory/get_large_hit
                        time:   [322.22 ns 327.31 ns 334.36 ns]
Found 3 outliers among 50 measurements (6.00%)
  2 (4.00%) high mild
  1 (2.00%) high severe

backend/in_memory/set_small
                        time:   [670.99 ns 675.85 ns 683.51 ns]
Found 6 outliers among 50 measurements (12.00%)
  1 (2.00%) high mild
  5 (10.00%) high severe

backend/in_memory/set_large
                        time:   [659.02 ns 660.32 ns 661.77 ns]
Found 6 outliers among 50 measurements (12.00%)
  2 (4.00%) high mild
  4 (8.00%) high severe

stampede/cache_layer    time:   [5.8962 ms 5.9213 ms 5.9453 ms]
Found 4 outliers among 50 measurements (8.00%)
  2 (4.00%) low mild
  1 (2.00%) high mild
  1 (2.00%) high severe

stampede/no_cache       time:   [5.7514 ms 5.7645 ms 5.7773 ms]
Found 2 outliers among 50 measurements (4.00%)
  1 (2.00%) low mild
  1 (2.00%) high mild

stampede/backend_calls  time:   [857.68 ps 865.73 ps 874.95 ps]
Found 3 outliers among 50 measurements (6.00%)
  3 (6.00%) high mild

stale_while_revalidate/stale_hit_latency
                        time:   [33.511 ms 33.597 ms 33.681 ms]
Found 1 outliers among 50 measurements (2.00%)
  1 (2.00%) low mild

stale_while_revalidate/strict_refresh_latency
                        time:   [33.612 ms 33.694 ms 33.772 ms]
Found 1 outliers among 50 measurements (2.00%)
  1 (2.00%) low mild

codec/bincode_encode_small
                        time:   [360.62 ns 361.96 ns 363.40 ns]
Found 1 outliers among 50 measurements (2.00%)
  1 (2.00%) high mild

codec/bincode_decode_small
                        time:   [376.15 ns 380.91 ns 390.33 ns]
Found 5 outliers among 50 measurements (10.00%)
  5 (10.00%) high severe

codec/bincode_encode_large
                        time:   [146.04 µs 146.75 µs 147.94 µs]
Found 5 outliers among 50 measurements (10.00%)
  5 (10.00%) high severe

codec/bincode_decode_large
                        time:   [172.75 µs 173.83 µs 174.73 µs]
Found 5 outliers among 50 measurements (10.00%)
  1 (2.00%) low severe
  4 (8.00%) low mild

negative_cache/initial_miss
                        time:   [14.004 µs 14.049 µs 14.099 µs]
Found 6 outliers among 50 measurements (12.00%)
  6 (12.00%) high mild

negative_cache/stored_negative_hit
                        time:   [21.819 ms 21.870 ms 21.920 ms]
Found 5 outliers among 50 measurements (10.00%)
  4 (8.00%) low mild
  1 (2.00%) high mild

negative_cache/after_ttl_churn
                        time:   [5.6481 µs 5.6625 µs 5.6802 µs]
Found 2 outliers among 50 measurements (4.00%)
  2 (4.00%) high mild

negative_cache/backend_calls
                        time:   [254.62 ps 257.71 ps 261.60 ps]
Found 7 outliers among 50 measurements (14.00%)
  6 (12.00%) high mild
  1 (2.00%) high severe

```

