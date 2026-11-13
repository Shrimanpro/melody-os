use bootloader::bootinfo::{MemoryMap, MemoryRegionType};
use x86_64::{
    PhysAddr, VirtAddr,
    structures::paging::{
        FrameAllocator, Mapper, OffsetPageTable, Page, PageTable, PageTableFlags, PhysFrame,
        Size4KiB, Translate,
    },
};

pub static PHYSICAL_MEMORY_OFFSET: spin::Mutex<Option<VirtAddr>> = spin::Mutex::new(None);
pub static FRAME_ALLOCATOR: spin::Mutex<Option<BootInfoFrameAllocator>> = spin::Mutex::new(None);

/// Translate any mapped virtual address to its physical address.
pub fn virt_to_phys(virt: VirtAddr) -> Option<PhysAddr> {
    let offset = (*PHYSICAL_MEMORY_OFFSET.lock())?;
    let mapper = unsafe { OffsetPageTable::new(active_level_4_table(offset), offset) };
    mapper.translate_addr(virt)
}

/// Make sure a physical MMIO range is reachable at `physical_memory_offset + phys_addr`
/// and return that virtual address. The bootloader only maps physical memory up to the
/// highest RAM address, so PCI BARs above it need extra uncached mappings.
pub fn ensure_mmio_mapped(phys_addr: u64, size: usize) -> VirtAddr {
    let offset = PHYSICAL_MEMORY_OFFSET
        .lock()
        .expect("PHYSICAL_MEMORY_OFFSET not initialized");
    let virt_base = offset + phys_addr;

    unsafe {
        let mut mapper = OffsetPageTable::new(active_level_4_table(offset), offset);
        let mut alloc_guard = FRAME_ALLOCATOR.lock();

        let start_frame = PhysFrame::<Size4KiB>::containing_address(PhysAddr::new(phys_addr));
        let end_frame = PhysFrame::<Size4KiB>::containing_address(PhysAddr::new(
            phys_addr.saturating_add(size.max(4096) as u64 - 1),
        ));

        for frame in PhysFrame::range_inclusive(start_frame, end_frame) {
            let page = Page::<Size4KiB>::containing_address(offset + frame.start_address().as_u64());
            if mapper.translate_addr(page.start_address()).is_none() {
                if let Some(ref mut fa) = *alloc_guard {
                    let flags = PageTableFlags::PRESENT
                        | PageTableFlags::WRITABLE
                        | PageTableFlags::NO_CACHE
                        | PageTableFlags::WRITE_THROUGH;
                    if let Ok(tlb) = mapper.map_to(page, frame, flags, fa) {
                        tlb.flush();
                    }
                }
            }
        }
    }

    virt_base
}

pub fn register_frame_allocator(fa: BootInfoFrameAllocator) {
    *FRAME_ALLOCATOR.lock() = Some(fa);
}

/// A physically contiguous, page-aligned, zeroed block of memory for device DMA.
/// It is never freed: drivers allocate their rings once at boot.
#[derive(Clone, Copy)]
pub struct DmaRegion {
    pub virt: *mut u8,
    pub phys: u64,
    pub size: usize,
}

unsafe impl Send for DmaRegion {}
unsafe impl Sync for DmaRegion {}

impl DmaRegion {
    pub fn phys_at(&self, offset: usize) -> u64 {
        self.phys + offset as u64
    }

    pub fn ptr_at<T>(&self, offset: usize) -> *mut T {
        unsafe { self.virt.add(offset) as *mut T }
    }

    pub fn as_slice(&self) -> &'static mut [u8] {
        unsafe { core::slice::from_raw_parts_mut(self.virt, self.size) }
    }
}

/// Allocate `pages` physically contiguous 4 KiB frames below 4 GiB, reachable through the
/// bootloader's physical memory window.
pub fn alloc_dma(pages: usize) -> Result<DmaRegion, &'static str> {
    let offset = (*PHYSICAL_MEMORY_OFFSET.lock()).ok_or("memory not initialized")?;
    let mut guard = FRAME_ALLOCATOR.lock();
    let fa = guard.as_mut().ok_or("frame allocator not registered")?;
    let phys = fa.allocate_contiguous(pages).ok_or("out of contiguous DMA memory")?;
    if phys + (pages as u64) * 4096 > 0x1_0000_0000 {
        return Err("DMA memory above 4 GiB");
    }
    let virt = (offset + phys).as_mut_ptr::<u8>();
    unsafe { core::ptr::write_bytes(virt, 0, pages * 4096) };
    Ok(DmaRegion { virt, phys, size: pages * 4096 })
}

pub unsafe fn init(physical_memory_offset: VirtAddr) -> OffsetPageTable<'static> {
    *PHYSICAL_MEMORY_OFFSET.lock() = Some(physical_memory_offset);
    unsafe { OffsetPageTable::new(active_level_4_table(physical_memory_offset), physical_memory_offset) }
}

unsafe fn active_level_4_table(physical_memory_offset: VirtAddr) -> &'static mut PageTable {
    use x86_64::registers::control::Cr3;

    let (level_4_table_frame, _) = Cr3::read();
    let virt = physical_memory_offset + level_4_table_frame.start_address().as_u64();
    unsafe { &mut *virt.as_mut_ptr::<PageTable>() }
}

/// Hands out usable frames from the bootloader memory map in ascending order.
/// Keeps a cursor (region index + next address) so each allocation is O(1).
pub struct BootInfoFrameAllocator {
    memory_map: &'static MemoryMap,
    region: usize,
    next_addr: u64,
}

impl BootInfoFrameAllocator {
    pub unsafe fn init(memory_map: &'static MemoryMap) -> Self {
        let mut fa = BootInfoFrameAllocator { memory_map, region: 0, next_addr: 0 };
        fa.seek_usable();
        fa
    }

    /// Move the cursor to the next usable region if the current one is exhausted.
    fn seek_usable(&mut self) {
        while self.region < self.memory_map.len() {
            let r = &self.memory_map[self.region];
            if r.region_type == MemoryRegionType::Usable {
                let start = r.range.start_addr().max(self.next_addr);
                // Skip the first MiB entirely (BIOS, VGA, real-mode leftovers).
                let start = start.max(0x10_0000);
                let start = (start + 4095) & !4095;
                if start + 4096 <= r.range.end_addr() {
                    self.next_addr = start;
                    return;
                }
            }
            self.region += 1;
        }
    }

    /// Allocate `count` frames that are physically adjacent. Frames skipped at the tail
    /// of a too-small region are simply abandoned (only happens a handful of times at boot).
    pub fn allocate_contiguous(&mut self, count: usize) -> Option<u64> {
        let bytes = count as u64 * 4096;
        loop {
            self.seek_usable();
            if self.region >= self.memory_map.len() {
                return None;
            }
            let end = self.memory_map[self.region].range.end_addr();
            if self.next_addr + bytes <= end {
                let addr = self.next_addr;
                self.next_addr += bytes;
                return Some(addr);
            }
            self.region += 1;
        }
    }
}

unsafe impl FrameAllocator<Size4KiB> for BootInfoFrameAllocator {
    fn allocate_frame(&mut self) -> Option<PhysFrame> {
        self.allocate_contiguous(1)
            .map(|addr| PhysFrame::containing_address(PhysAddr::new(addr)))
    }
}
