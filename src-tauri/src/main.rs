// A console-subsystem release build opens an extra console window on
// Windows. This attribute suppresses that console window. Do not remove the
// attribute.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    firstmate_lib::run()
}
