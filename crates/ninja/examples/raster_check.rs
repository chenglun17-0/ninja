//! 字形覆盖率取证：位图应该是字形形状，不是整版实心/整版空。
use ninja::font::{Font, Weight};

fn main() {
    let mut font = Font::new(13.0, 2.0);
    let max_w = 4.0 * font.metrics.cell_w * font.scale;
    for text in ["M", "g", "A", "n", "|"] {
        let g = font.rasterize(text, Weight::Regular, max_w).expect("rasterize");
        let total = g.coverage.len();
        let solid = g.coverage.iter().filter(|&&c| c > 200).count();
        let blank = g.coverage.iter().filter(|&&c| c < 30).count();
        let mid = total - solid - blank;
        println!(
            "{text:?}: {total}px solid={solid} blank={blank} mid={mid} (w={} h={})",
            g.w, g.h
        );
        // 打印 'A' 的 ASCII 覆盖图
        if text == "M" {
            for y in 0..g.h as usize {
                let mut line = String::new();
                for x in 0..g.w as usize {
                    let c = g.coverage[y * g.w as usize + x];
                    line.push(if c > 160 { '#' } else if c > 60 { '.' } else { ' ' });
                }
                println!("  |{line}|");
            }
        }
    }
}
