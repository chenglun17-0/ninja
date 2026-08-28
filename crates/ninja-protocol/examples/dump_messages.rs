//! 打印每条消息的黄金 JSON（type 一行）：第二语言实现者的字节参照，
//! 也是 tests/golden/*.json 的再生成源。
//!
//! ```sh
//! cargo run -p ninja-protocol --example dump_messages \
//!     | while IFS=$'\t' read -r name json; do
//!         printf '%s' "$json" > "crates/ninja-protocol/tests/golden/$name.json";
//!       done
//! ```

use ninja_protocol::Message;

fn main() {
    for m in Message::sample_messages() {
        println!("{}\t{}", m.message_type(), m.to_json().unwrap());
    }
}
