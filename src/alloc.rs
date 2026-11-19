//! macOS: large blocks straight from mmap/munmap.
//!
//! The system allocator keeps freed large blocks in a cache instead of returning them, and a sync
//! allocates and frees ~100 MB FFT buffers dozens of times: the process footprint stayed at several GB
//! long after the work was done (with only a few MB actually in use). Blocks of LARGE bytes or more
//! bypass malloc, so freeing them gives the memory back to the system at once. Everything else goes
//! to the system allocator unchanged.

use std::alloc::{GlobalAlloc, Layout, System};
use std::ffi::c_void;

const LARGE: usize = 4 << 20;
/// mmap returns page-aligned memory (4 KB on Intel, 16 KB on Apple silicon).
const MAX_ALIGN: usize = 4096;

const PROT_READ: i32 = 0x01;
const PROT_WRITE: i32 = 0x02;
const MAP_PRIVATE: i32 = 0x0002;
const MAP_ANON: i32 = 0x1000;

extern "C" {
    fn mmap(addr: *mut c_void, len: usize, prot: i32, flags: i32, fd: i32, offset: i64) -> *mut c_void;
    fn munmap(addr: *mut c_void, len: usize) -> i32;
}

fn is_large(l: &Layout) -> bool {
    l.size() >= LARGE && l.align() <= MAX_ALIGN
}

pub struct Alloc;

unsafe impl GlobalAlloc for Alloc {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        if !is_large(&l) {
            return System.alloc(l);
        }
        let p = mmap(std::ptr::null_mut(), l.size(), PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANON, -1, 0);
        if p as isize == -1 {
            std::ptr::null_mut()
        } else {
            p as *mut u8
        }
    }

    unsafe fn alloc_zeroed(&self, l: Layout) -> *mut u8 {
        if is_large(&l) {
            self.alloc(l) // anonymous mappings are already zeroed
        } else {
            System.alloc_zeroed(l)
        }
    }

    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        if is_large(&l) {
            munmap(p as *mut c_void, l.size());
        } else {
            System.dealloc(p, l)
        }
    }

    unsafe fn realloc(&self, p: *mut u8, l: Layout, new_size: usize) -> *mut u8 {
        let nl = Layout::from_size_align_unchecked(new_size, l.align());
        if !is_large(&l) && !is_large(&nl) {
            return System.realloc(p, l, new_size);
        }
        let q = self.alloc(nl);
        if !q.is_null() {
            std::ptr::copy_nonoverlapping(p, q, l.size().min(new_size));
            self.dealloc(p, l);
        }
        q
    }
}
