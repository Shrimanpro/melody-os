#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(melody_os::test_runner)]
#![reexport_test_harness_main = "test_main"]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use bootloader::{entry_point, BootInfo};
use core::panic::PanicInfo;
use melody_os::audio::player::Player;
use melody_os::net::mpd::{Client, GREETING};

entry_point!(main);

fn main(boot_info: &'static BootInfo) -> ! {
    melody_os::init_with_memory(boot_info);
    test_main();
    melody_os::hlt_loop();
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    melody_os::test_panic_handler(info)
}

/// Send one chunk of protocol text and return what the server answered.
fn exchange(client: &mut Client, player: &mut Player, input: &str) -> String {
    client.out.clear();
    client.feed(input.as_bytes(), player);
    String::from_utf8(core::mem::take(&mut client.out)).unwrap()
}

#[test_case]
fn greets_and_reports_status() {
    let mut player = Player::new(Vec::new(), None);
    let mut client = Client::new();
    assert_eq!(&client.out[..], GREETING);
    let resp = exchange(&mut client, &mut player, "status\n");
    assert!(resp.contains("state: stop\n"));
    assert!(resp.contains("playlistlength: 0\n"));
    assert!(resp.ends_with("OK\n"));
}

#[test_case]
fn unknown_command_is_acked() {
    let mut player = Player::new(Vec::new(), None);
    let mut client = Client::new();
    let resp = exchange(&mut client, &mut player, "frobnicate\n");
    assert!(resp.starts_with("ACK [5@0] {frobnicate}"));
}

#[test_case]
fn command_lists_and_volume() {
    let mut player = Player::new(Vec::new(), None);
    let mut client = Client::new();
    let resp = exchange(&mut client, &mut player, "command_list_ok_begin\nsetvol 42\ngetvol\ncommand_list_end\n");
    assert_eq!(resp, "list_OK\nvolume: 42\nlist_OK\nOK\n");
    let resp = exchange(&mut client, &mut player, "setvol 200\n");
    assert!(resp.starts_with("ACK [2@0]"));
}
