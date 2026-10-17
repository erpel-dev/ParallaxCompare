// A tiny no_std library compiled to WebAssembly: exports, an import, a global,
// a data segment, a loop and an indirect call.
#![no_std]

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    core::arch::wasm32::unreachable()
}

#[link(wasm_import_module = "env")]
unsafe extern "C" {
    fn host_log(value: i32);
}

static GREETING: &[u8] = b"Hello from counter module v2";
static mut COUNT: i32 = 0;

#[unsafe(no_mangle)]
pub extern "C" fn increment(by: i32) -> i32 {
    unsafe {
        COUNT += by;
        host_log(COUNT);
        COUNT
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn sum_to(n: i32) -> i64 {
    let mut total = 0i64;
    let mut i = 0;
    while i < n {
        total += i64::from(i) * 2;
        i += 1;
    }
    total
}

#[unsafe(no_mangle)]
pub extern "C" fn greeting_len() -> usize {
    GREETING.len()
}

#[inline(never)]
fn double(x: i32) -> i32 {
    x * 2
}

#[inline(never)]
fn square(x: i32) -> i32 {
    x * x
}

#[unsafe(no_mangle)]
pub extern "C" fn apply(op: i32, x: i32) -> i32 {
    let table: [fn(i32) -> i32; 2] = [double, square];
    table[(op as usize) & 1](x)
}

#[unsafe(no_mangle)]
pub extern "C" fn reset() {
    unsafe { COUNT = 0 }
}

#[unsafe(no_mangle)]
pub extern "C" fn greeting_byte(i: usize) -> u8 {
    GREETING.get(i).copied().unwrap_or(0)
}
