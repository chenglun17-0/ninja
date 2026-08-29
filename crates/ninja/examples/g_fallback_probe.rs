//! G-字形回退 · 验收取证（复跑）：验收样本逐类像素探针（非空白非豆腐）
//! + 每样本解析到的字体。运行：cargo run -p ninja --example g_fallback_probe
use ninja::font::{Font, Weight};

struct Category(&'static str, Vec<&'static str>);

fn main() {
    let mut font = Font::new(13.0, 2.0);
    let max_w = 4.0 * font.metrics.cell_w * font.scale;
    let cats = vec![
        Category("box-drawing", vec!["│","┌","┐","└","┘","├","┤","┬","┴","┼","═","║"]),
        Category("symbols", vec!["→","←","⇄","✓","✗","●","▲","△","◆","★","☆"]),
        Category("powerline", vec!["\u{E0B0}","\u{E0B1}","\u{E0B2}","\u{E0B3}"]),
        Category("cjk", vec!["中","文","日","本","か","な","漢"]),
        Category("emoji", vec!["😀","🎉","👍"]),
        Category("latin-ext", vec!["é","à","ü","ß","ñ","ő"]),
        Category("greek", vec!["Ω","α","β","λ"]),
        Category("cyrillic", vec!["П","р","и","в","е","т"]),
    ];
    let mut failures = 0usize;
    for Category(name, samples) in &cats {
        for s in samples {
            let ps = font.font_postscript_of(s);
            match font.rasterize(s, Weight::Regular, max_w) {
                None => {
                    println!("[FAIL] {name} {s:?} ({ps}): rasterize None");
                    failures += 1;
                }
                Some(g) => {
                    let ink = g.coverage.iter().filter(|&&c| c > 40).count();
                    let tofu = ps == "LastResort";
                    if ink == 0 || tofu {
                        println!(
                            "[FAIL] {name} {s:?} ({ps}): ink={ink} tofu={tofu} ({}x{})",
                            g.w, g.h
                        );
                        failures += 1;
                    } else {
                        println!("[ ok ] {name} {s:?} ({ps}): ink={ink} ({}x{})", g.w, g.h);
                    }
                }
            }
        }
    }
    println!("residuals: {:?}", font.residuals());
    println!("failures: {failures}");
}
