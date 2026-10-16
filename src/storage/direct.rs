//! 脚本执行占用的线程上, 存储调用直接进入引擎, 不再为每次读取另开 `spawn_blocking`.

use std::cell::Cell;

thread_local! {
    static DIRECT: Cell<bool> = const { Cell::new(false) };
}

#[allow(dead_code)]
pub(crate) struct DirectStorageIo {
    previous: bool,
}

impl DirectStorageIo {
    #[allow(dead_code)]
    pub(crate) fn enter() -> Self {
        let previous = DIRECT.with(|flag| {
            let previous = flag.get();
            flag.set(true);
            previous
        });
        Self { previous }
    }
}

impl Drop for DirectStorageIo {
    fn drop(&mut self) {
        DIRECT.with(|flag| flag.set(self.previous));
    }
}

pub(crate) fn storage_io_is_direct() -> bool {
    DIRECT.with(|flag| flag.get())
}
