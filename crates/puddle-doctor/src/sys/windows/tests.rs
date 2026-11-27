// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;
use windows_sys::Win32::Foundation::CloseHandle;
use windows_sys::Win32::System::JobObjects::{CreateJobObjectW, SetInformationJobObject};

#[test]
fn probes_read_what_an_account_without_admin_rights_may_read() {
    // Values depend on the machine; what does not is whether the read worked.
    let _hv = hypervisor();
    assert!(
        matches!(code_integrity(), Some(CodeIntegrityFacts::Read(_))),
        "the code-integrity query failed: {:?}",
        code_integrity()
    );
    // A process in no job gives `None`; one in a job must have had it read.
    assert!(
        !matches!(job(), Some(JobFacts::Unreadable { .. })),
        "{:?}",
        job()
    );
    assert!(problematic_software().is_some());
    // `None` is "no such service", which is a fact of the machine.
    let _service = ssh_agent_service();
}

#[test]
fn the_job_limit_length_is_the_struct_size() {
    assert_eq!(
        JOB_LIMIT_INFO_LEN as usize,
        size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>()
    );
}

/// A job made here with `limit_flags`, read back through `job_facts` as the probe reads it.
#[expect(unsafe_code, reason = "job object calls on a job the test owns")]
fn facts_of_a_new_job(limit_flags: u32) -> JobFacts {
    // SAFETY: no security attributes, no name.
    let job = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
    assert!(!job.is_null(), "{}", io::Error::last_os_error());
    // SAFETY: all-zero is a valid value of this plain-data struct.
    let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
    info.BasicLimitInformation.LimitFlags = limit_flags;
    // SAFETY: `job` is the open handle made above; `info` is the struct the class documents,
    // with its size.
    let set = unsafe {
        SetInformationJobObject(
            job,
            JobObjectExtendedLimitInformation,
            (&raw const info).cast(),
            JOB_LIMIT_INFO_LEN,
        )
    };
    assert_ne!(set, 0, "{}", io::Error::last_os_error());
    let facts = job_facts(job);
    // SAFETY: closes the handle made above, once.
    unsafe { CloseHandle(job) };
    facts
}

#[test]
fn a_job_that_allows_breakaway_reads_back_as_such() {
    assert_eq!(
        facts_of_a_new_job(JOB_OBJECT_LIMIT_BREAKAWAY_OK),
        JobFacts::Read {
            breakaway_allowed: true
        }
    );
    assert_eq!(
        facts_of_a_new_job(JOB_OBJECT_LIMIT_SILENT_BREAKAWAY_OK),
        JobFacts::Read {
            breakaway_allowed: true
        }
    );
}

#[test]
fn a_job_without_breakaway_reads_back_as_forbidding_it() {
    assert_eq!(
        facts_of_a_new_job(0),
        JobFacts::Read {
            breakaway_allowed: false
        }
    );
}

#[test]
fn hypervisor_present_reads_the_first_bool() {
    assert!(!hypervisor_present(0));
    assert!(hypervisor_present(1));
    assert!(!hypervisor_present(0xFFFF_FFFF_0000_0000));
}

#[test]
fn wide_strings_are_nul_terminated() {
    assert_eq!(wide("ab"), [97, 98, 0]);
}
