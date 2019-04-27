/*!
This example demonstrates some strategies to prevent the Capcom from being detected by anti-cheats.

# Bypass MmUnloadedDrivers

UnknownCheats thread: https://www.unknowncheats.me/forum/anti-cheat-bypass/231400-clearing-mmunloadeddrivers-mmlastunloadeddriver-2.html#post2210153

Set `LDR_DATA_TABLE_ENTRY.BaseDllName.Length` to zero to prevent MiRememberUnloadedDriver from recording the unloaded driver.

# Bypass PiDDBCacheTable

UnknownCheats thread: https://www.unknowncheats.me/forum/anti-cheat-bypass/324665-clearing-piddbcachetable.html

 */

#![allow(non_snake_case, non_camel_case_types)]

use std::{mem, ops::Range};

use winapi::shared::ntdef::{LIST_ENTRY, UNICODE_STRING, ULONG, NTSTATUS};
use ntapi::ntldr::PLDR_DATA_TABLE_ENTRY;

use pelite::pattern as pat;
use pelite::pe64::*;
use pelite::pe64::exports::GetProcAddress;

use obfstr::wide;

#[derive(Copy, Clone)]
#[repr(C)]
struct PiDDBCacheEntry
{
	List: LIST_ENTRY,
	DriverName: UNICODE_STRING,
	TimeDateStamp: ULONG,
	LoadStatus: NTSTATUS,
	_0x0028: [u8; 16], // data from the shim engine, or uninitialized memory for custom drivers
}
impl PiDDBCacheEntry {
	fn new(driver_name: &[u16], time_date_stamp: u32) -> PiDDBCacheEntry {
		PiDDBCacheEntry {
			DriverName: capcom0::unicode_string(driver_name),
			TimeDateStamp: time_date_stamp,
			..unsafe { mem::zeroed() }
		}
	}
}

// Helper to call MmGetSystemRoutineAddress and cast the return value appropriately
macro_rules! get_system_routine_address {
	($ctx:expr, $ty:ty, $ws:expr) => {{
		match $ws {
			ws => {
				let mut us = capcom0::unicode_string(ws);
				mem::transmute::<_, $ty>(($ctx.get_system_routine_address)(&mut us))
			}
		}
	}};
}

// Get the virtual address range of the named section
fn section_range(file: PeFile, name: &[u8; 8]) -> Range<u32> {
	match file.section_headers().iter().find(|sect| &sect.Name == name) {
		Some(sect) => sect.VirtualAddress..sect.VirtualAddress + sect.VirtualSize,
		None => 0..file.optional_header().SizeOfImage,
	}
}

type PERESOURCE = usize;
type BOOLEAN = u8;
type PRTL_AVL_TABLE = usize;

fn main() {
	// Capcom image
	let capcom_file = PeFile::from_bytes(&capcom0::Driver::image()[..]).unwrap();
	let time_date_stamp = capcom_file.file_header().TimeDateStamp;

	// NTOSKRNL image
	let ntos_map = pelite::FileMap::open(r"C:\Windows\System32\ntoskrnl.exe").unwrap();
	let ntos_file = PeFile::from_bytes(&ntos_map).unwrap();

	let init_range = section_range(ntos_file, b"INIT\0\0\0\0");
	let page_range = section_range(ntos_file, b"PAGE\0\0\0\0");
	let mut save = [0; 4];

	// Find MiLookupDataTableEntry offset
	assert!(ntos_file.scanner().finds(&pat::parse("BA 01 00 00 00 48 8B F1 E8 $ '").unwrap(), init_range.clone(), &mut save), "MiLookupDataTableEntry not found");
	let lookup_offset = save[1] as usize;

	// Find PiDDBCacheLock and PiDDBCacheTable offsets
	assert!(ntos_file.scanner().finds(&pat::parse("48 89 40 08 48 8D 0D $ '").unwrap(), init_range, &mut save), "Cannot find PiDDBCacheLock!");
	let lock_offset = save[1] as usize;
	assert!(ntos_file.scanner().finds(&pat::parse("66 03 D2 48 8D 0D $ '").unwrap(), page_range, &mut save), "Cannot find PiDDBCacheTable!");
	let table_offset = save[1] as usize;

	println!("ntosknrl.exe!{:#x} MiLookupDataTableEntry", lookup_offset);
	println!("ntoskrnl.exe!{:#x} PiDDBCacheLock", lock_offset);
	println!("ntoskrnl.exe!{:#x} PiDDBCacheTable", table_offset);

	// Delta to ntosknrl image base
	let ntos_delta = ntos_file.get_export("MmGetSystemRoutineAddress").unwrap().symbol().unwrap() as usize;

	let result = capcom0::setup(|driver, device| {
		let mut capcom_base = 0;
		let mut ntos_base = 0;

		let driver_name = driver.file_name();
		println!("DriverName: {}", String::from_utf16_lossy(driver_name));

		// Note: wrapping arithmetic to avoid overflow checks
		// Note: because we are in capcom land we cannot take locks
		// Note: not much we can do about that
		unsafe {
			device.elevate(|ctx| {
				capcom_base = ctx.capcom_base;
				ntos_base = (ctx.get_system_routine_address as usize).wrapping_sub(ntos_delta);

				let MiLookupDataTableEntry:
					unsafe extern "system" fn(usize, i32) -> PLDR_DATA_TABLE_ENTRY =
					mem::transmute(ntos_base.wrapping_add(lookup_offset));

				// Get Capcom's LDR_DATA_TABLE_ENTRY without locking
				// Overwrite BaseDllName.Length to zero so MiRememberUnloadedDriver breaks out early
				let capcom_dte = MiLookupDataTableEntry(ctx.capcom_base, 0);
				if !capcom_dte.is_null() {
					(*capcom_dte).BaseDllName.Length = 0;
				}

				let ExAcquireResourceExclusiveLite = get_system_routine_address!(ctx,
					unsafe extern "system" fn(PERESOURCE, BOOLEAN) -> BOOLEAN,
					wide!("ExAcquireResourceExclusiveLite"));
				let ExReleaseResourceLite = get_system_routine_address!(ctx,
					unsafe extern "system" fn(PERESOURCE),
					wide!("ExReleaseResourceLite"));
				let RtlLookupElementGenericTableAvl = get_system_routine_address!(ctx,
					unsafe extern "system" fn(PRTL_AVL_TABLE, *mut PiDDBCacheEntry) -> *mut PiDDBCacheEntry,
					wide!("RtlLookupElementGenericTableAvl"));
				let RtlDeleteElementGenericTableAvl = get_system_routine_address!(ctx,
					unsafe extern "system" fn(PRTL_AVL_TABLE, *mut PiDDBCacheEntry),
					wide!("RtlDeleteElementGenericTableAvl"));

				let PiDDBLock = ntos_base.wrapping_add(lock_offset);
				let PiDDBCache = ntos_base.wrapping_add(table_offset);

				// Locking is... counter-productive in the Capcom hell, oh well
				ExAcquireResourceExclusiveLite(PiDDBLock, 1);
				let mut lookup_entry = PiDDBCacheEntry::new(driver_name, time_date_stamp);
				let found_entry = RtlLookupElementGenericTableAvl(PiDDBCache, &mut lookup_entry);
				if !found_entry.is_null() {
					// Unlink the entry
					let Flink = (*found_entry).List.Flink;
					let Blink = (*found_entry).List.Blink;
					(*Flink).Blink = Blink;
					(*Blink).Flink = Flink;
					// Remove from the table
					RtlDeleteElementGenericTableAvl(PiDDBCache, found_entry);
				}
				ExReleaseResourceLite(PiDDBLock);
			});
		}

		println!("CapcomBase was {:#x}", capcom_base);
		println!("NtosBase was {:#x}", ntos_base);
	});

	match result {
		Ok(()) => println!("Success!"),
		Err(err) => println!("Error: {}", err),
	}
}