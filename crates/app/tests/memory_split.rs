//! The soak's memory split (soak 40139563554's drift, 2026-10-25): the compositor's own memory from
//! `/proc/self/status` and CEF's children's PSS by Chromium's `--type=`, which the driver logs once a
//! minute as `memory split …` beside the beat's unchanged `memMb`. Every expected line here is typed
//! from the line's specification (the driver's doc), never built by the code under test, and an
//! absent value reads `-`, never 0. A kind CEF listed and could not read is `listed` with `n=0` and
//! `kb=-`, never a 0 PSS (PR #82 review I2: that is the vacuous mode that reads as "CEF is flat").
use app::driver::memory_line;
use app::ports::{CefPoll, CefPss, KindPss, MemorySplit, ProcStatus};

fn kind(kb: u64, n: u32, listed: u32) -> Option<KindPss> {
    Some(KindPss { kb, n, listed })
}

/// Each child's PSS lands under its own `--type=`: summed, and counted as read; every child counts
/// as listed. A child whose PSS was not read (gone between the listing and its `smaps_rollup`, or
/// its rollup unreadable) adds no PSS and is not counted as read, so a kind with only such children
/// is listed with none read, never absent and never a 0 read; a `--type=` none of the four names is
/// `other`.
#[test]
fn cef_pss_buckets_each_childs_pss_by_its_type() {
    let empty = CefPss::default();
    assert_eq!(
        empty,
        CefPss {
            renderer: None,
            gpu_process: None,
            utility: None,
            zygote: None,
            other: None,
        },
        "no child: every kind absent"
    );
    let children: [(&str, Option<u64>); 10] = [
        ("renderer", Some(100_000)),
        ("renderer", Some(50_000)),
        ("gpu-process", Some(30_000)),
        ("utility", Some(20_000)),
        ("utility", None),
        ("zygote", Some(10_000)),
        ("zygote", Some(5_000)),
        ("broker", Some(7_000)),
        // A zero is a measured zero (a kernel's empty rollup), counted, never dropped.
        ("broker", Some(0)),
        ("gpu-process", None),
    ];
    let mut p = CefPss::default();
    for (k, kb) in children {
        p.add(k, kb);
    }
    assert_eq!(
        p,
        CefPss {
            renderer: kind(150_000, 2, 2),
            gpu_process: kind(30_000, 1, 2),
            utility: kind(20_000, 1, 2),
            zygote: kind(15_000, 2, 2),
            other: kind(7_000, 2, 2),
        }
    );
    let mut unread = CefPss::default();
    unread.add("renderer", None);
    unread.add("zygote", None);
    unread.add("zygote", None);
    assert_eq!(
        unread,
        CefPss {
            renderer: kind(0, 0, 1),
            zygote: kind(0, 0, 2),
            ..CefPss::default()
        },
        "unread children only: listed, none read"
    );
    eprintln!("children bucketed: {}", children.len());
}

/// The line, field by field: the input generation, the beat's tree total, the compositor's five
/// `/proc/self/status` fields, the CEF poll's number, and each `--type=`'s PSS, read count and listed
/// count. Absent is `-`; a kind the poll found none of reads `kb=- n=0 listed=0`; a kind it listed and
/// read none of reads `kb=- n=0 listed=<k>`; with no poll at all, every CEF field is `-`. A measured 0
/// prints as 0.
#[test]
fn the_memory_line_names_every_field_and_writes_an_absent_one_as_a_dash() {
    let full = MemorySplit {
        status: ProcStatus {
            vm_rss_kb: Some(812_345),
            rss_anon_kb: Some(700_001),
            rss_file_kb: Some(100_002),
            rss_shmem_kb: Some(12_342),
            threads: Some(87),
        },
        cef: Some(CefPoll {
            poll: 1_234,
            pss: CefPss {
                renderer: kind(200_000, 1, 1),
                gpu_process: kind(90_000, 1, 1),
                utility: kind(40_000, 2, 2),
                zygote: kind(15_000, 2, 2),
                other: None,
            },
        }),
    };
    // (name, split, tree_mb, input_gen, the line)
    type Case = (&'static str, MemorySplit, Option<u64>, Option<u64>, &'static str);
    let cases: [Case; 5] = [
        (
            "everything measured",
            full,
            Some(990),
            Some(3),
            "memory split input_gen=3 tree_mb=990 rss_kb=812345 anon_kb=700001 file_kb=100002 \
             shmem_kb=12342 threads=87 cef_poll=1234 cef_renderer_kb=200000 cef_renderer_n=1 \
             cef_renderer_listed=1 cef_gpu_process_kb=90000 cef_gpu_process_n=1 cef_gpu_process_listed=1 \
             cef_utility_kb=40000 cef_utility_n=2 cef_utility_listed=2 cef_zygote_kb=15000 cef_zygote_n=2 \
             cef_zygote_listed=2 cef_other_kb=- cef_other_n=0 cef_other_listed=0",
        ),
        (
            "nothing measured: no rebuild yet, no tree, no status line, no poll",
            MemorySplit::default(),
            None,
            None,
            "memory split input_gen=- tree_mb=- rss_kb=- anon_kb=- file_kb=- shmem_kb=- threads=- \
             cef_poll=- cef_renderer_kb=- cef_renderer_n=- cef_renderer_listed=- cef_gpu_process_kb=- \
             cef_gpu_process_n=- cef_gpu_process_listed=- cef_utility_kb=- cef_utility_n=- \
             cef_utility_listed=- cef_zygote_kb=- cef_zygote_n=- cef_zygote_listed=- cef_other_kb=- \
             cef_other_n=- cef_other_listed=-",
        ),
        (
            "some status lines missing, a poll that found no child",
            MemorySplit {
                status: ProcStatus {
                    vm_rss_kb: Some(4_096),
                    rss_anon_kb: None,
                    rss_file_kb: Some(1_024),
                    rss_shmem_kb: None,
                    threads: Some(1),
                },
                cef: Some(CefPoll {
                    poll: 1,
                    pss: CefPss::default(),
                }),
            },
            Some(4),
            Some(1),
            "memory split input_gen=1 tree_mb=4 rss_kb=4096 anon_kb=- file_kb=1024 shmem_kb=- threads=1 \
             cef_poll=1 cef_renderer_kb=- cef_renderer_n=0 cef_renderer_listed=0 cef_gpu_process_kb=- \
             cef_gpu_process_n=0 cef_gpu_process_listed=0 cef_utility_kb=- cef_utility_n=0 \
             cef_utility_listed=0 cef_zygote_kb=- cef_zygote_n=0 cef_zygote_listed=0 cef_other_kb=- \
             cef_other_n=0 cef_other_listed=0",
        ),
        (
            "measured zeros",
            MemorySplit {
                status: ProcStatus {
                    vm_rss_kb: Some(0),
                    rss_anon_kb: Some(0),
                    rss_file_kb: Some(0),
                    rss_shmem_kb: Some(0),
                    threads: Some(0),
                },
                cef: Some(CefPoll {
                    poll: 0,
                    pss: CefPss {
                        other: kind(0, 1, 1),
                        ..CefPss::default()
                    },
                }),
            },
            Some(0),
            Some(0),
            "memory split input_gen=0 tree_mb=0 rss_kb=0 anon_kb=0 file_kb=0 shmem_kb=0 threads=0 \
             cef_poll=0 cef_renderer_kb=- cef_renderer_n=0 cef_renderer_listed=0 cef_gpu_process_kb=- \
             cef_gpu_process_n=0 cef_gpu_process_listed=0 cef_utility_kb=- cef_utility_n=0 \
             cef_utility_listed=0 cef_zygote_kb=- cef_zygote_n=0 cef_zygote_listed=0 cef_other_kb=0 \
             cef_other_n=1 cef_other_listed=1",
        ),
        (
            "listed and not read: the PSS unknown (-), never the 0 it sums to; one of two read",
            MemorySplit {
                status: ProcStatus {
                    vm_rss_kb: Some(8_192),
                    ..ProcStatus::default()
                },
                cef: Some(CefPoll {
                    poll: 9,
                    pss: CefPss {
                        renderer: kind(0, 0, 1),
                        utility: kind(20_000, 1, 2),
                        zygote: kind(0, 0, 2),
                        ..CefPss::default()
                    },
                }),
            },
            Some(8),
            Some(2),
            "memory split input_gen=2 tree_mb=8 rss_kb=8192 anon_kb=- file_kb=- shmem_kb=- threads=- \
             cef_poll=9 cef_renderer_kb=- cef_renderer_n=0 cef_renderer_listed=1 cef_gpu_process_kb=- \
             cef_gpu_process_n=0 cef_gpu_process_listed=0 cef_utility_kb=20000 cef_utility_n=1 \
             cef_utility_listed=2 cef_zygote_kb=- cef_zygote_n=0 cef_zygote_listed=2 cef_other_kb=- \
             cef_other_n=0 cef_other_listed=0",
        ),
    ];
    let mut checked = 0;
    for (name, split, tree_mb, gen, want) in cases {
        assert_eq!(memory_line(&split, tree_mb, gen), want, "{name}");
        checked += 1;
    }
    eprintln!("memory lines checked: {checked}");
    assert_eq!(checked, 5);
}
