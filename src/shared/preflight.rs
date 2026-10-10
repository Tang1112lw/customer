//! 安装器「环境准备」清单：解析 `install.ps1 -CheckOnly` 的输出。
//!
//! 为什么检查逻辑在 PowerShell 里、这里只解析：真正干活的是 `install.ps1`，
//! 检查必须和它同一套口径（Windows 版本、盘符、注册表权限……在 Rust 里再写一遍
//! 只会多一处会走样的地方）。这里负责把结果变成界面能画、也能写进日志的结构。
//!
//! 输出协议（一行一条，其它输出一律忽略）：
//! ```text
//! CHECK|required|系统版本|pass|Windows 内部版本 22631（需要 17763 或更高）
//! CHECK|advisory|云端可达|fail|无法连接 http://…
//! ```
//! `required` 失败即阻断；`advisory` 只提醒。

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckItem {
    /// 必须项：不通过就不该继续安装。
    pub required: bool,
    pub title: String,
    pub passed: bool,
    pub detail: String,
}

/// 解析检查报告。无法识别的行直接丢掉（脚本会打别的日志，不该因此报错）。
pub fn parse(output: &str) -> Vec<CheckItem> {
    output
        .lines()
        .filter_map(|line| {
            let mut parts = line.trim().splitn(5, '|');
            if parts.next() != Some("CHECK") {
                return None;
            }
            let level = parts.next()?.trim();
            let title = parts.next()?.trim();
            let status = parts.next()?.trim();
            let detail = parts.next().unwrap_or("").trim();
            if title.is_empty() {
                return None;
            }
            Some(CheckItem {
                required: level == "required",
                title: title.to_owned(),
                passed: status == "pass",
                detail: detail.to_owned(),
            })
        })
        .collect()
}

/// 阻断项：必须项里没通过的。为空表示可以继续安装。
pub fn blockers(items: &[CheckItem]) -> Vec<&CheckItem> {
    items.iter().filter(|item| item.required && !item.passed).collect()
}

/// 供界面显示、以及"复制诊断"用的一句话总结。
pub fn headline(items: &[CheckItem]) -> String {
    let failed = blockers(items);
    if !failed.is_empty() {
        return format!("环境准备未通过：{} 项必须条件不满足", failed.len());
    }
    let warnings = items.iter().filter(|item| !item.required && !item.passed).count();
    if warnings > 0 {
        return format!("环境准备通过（{warnings} 项提醒，可以继续）");
    }
    format!("环境准备全部通过（{} 项检查）", items.len())
}

/// 纯文本报告：写进安装日志，也供"复制诊断"一键拷给技术支持。
pub fn diagnostic(items: &[CheckItem]) -> String {
    let mut text = String::from("AICRM 环境准备检查结果\n");
    text.push_str(&headline(items));
    text.push('\n');
    for item in items {
        let level = if item.required { "必须" } else { "提醒" };
        let status = if item.passed { "通过" } else { "未通过" };
        text.push_str(&format!("[{level}][{status}] {}：{}\n", item.title, item.detail));
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_check_lines_and_ignores_noise() {
        let output = "环境准备开始\n\
             CHECK|required|系统版本|pass|Windows 内部版本 22631\n\
             CHECK|advisory|云端可达|fail|无法连接 http://x\n\
             CHECK|required|磁盘空间|fail|C 盘可用 10MB\n\
             一些别的日志\n";
        let items = parse(output);
        assert_eq!(items.len(), 3);
        assert!(items[0].required && items[0].passed);
        assert!(!items[1].required && !items[1].passed);
        assert_eq!(items[2].title, "磁盘空间");
    }

    #[test]
    fn detail_may_contain_pipes_and_colons() {
        let items = parse("CHECK|required|安装目录可写|pass|C:\\x（拒绝访问 | 只读）");
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].detail, "C:\\x（拒绝访问 | 只读）");
    }

    #[test]
    fn only_required_failures_block() {
        let items = parse(
            "CHECK|required|系统版本|pass|x\nCHECK|advisory|微信已安装|fail|没找到微信\nCHECK|required|磁盘空间|fail|空间不足\n",
        );
        let failed = blockers(&items);
        assert_eq!(failed.len(), 1);
        assert_eq!(failed[0].title, "磁盘空间");
        assert!(headline(&items).contains("1 项必须条件"));
        assert!(diagnostic(&items).contains("[提醒][未通过] 微信已安装"));
    }

    #[test]
    fn missing_required_key_falls_back_to_advisory() {
        // 级别字段写成别的值时不该被当成必须项（宁可少拦，不可误拦）
        let items = parse("CHECK|unknown|某项|fail|x");
        assert_eq!(items.len(), 1);
        assert!(!items[0].required);
        assert!(blockers(&items).is_empty());
    }
}
