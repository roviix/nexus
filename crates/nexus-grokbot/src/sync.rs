//! 在同步上下文里跑一个 future。
//!
//! `nexus-sand` 的安装编排是同步的（一把闸、顺序步骤），但续期 / 读 pod 要发 HTTP。直接
//! `block_on` 在已经身处 tokio runtime 的线程上会 panic（Tauri 的命令线程就是），所以另起一条
//! 普通线程、在那上面建一个 current-thread runtime 跑完再 join——哪儿调都安全。

use std::future::Future;

pub fn run_sync<F, T>(fut: F) -> T
where
    F: Future<Output = T> + Send + 'static,
    T: Send + 'static,
{
    std::thread::scope(|s| {
        s.spawn(|| {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("build current-thread runtime");
            rt.block_on(fut)
        })
        .join()
        .expect("grokbot sync worker panicked")
    })
}
