// Integration tests for vocabulary expansion: political/regional proper nouns
// and additional IT/software terminology (Tier 4.1 + 4.2).
//
// Uses the full embedded ruleset via Scanner to ensure new rules integrate
// correctly with existing scanning logic.

use zhtw_mcp::engine::scan::Scanner;
use zhtw_mcp::rules::ruleset::{IssueType, PoliticalStance, Profile, Ruleset};

/// Build a scanner from the embedded ruleset (same as the MCP server uses).
fn full_scanner() -> Scanner {
    let json_str = include_str!("../assets/ruleset.json");
    let ruleset: Ruleset = serde_json::from_str(json_str).unwrap();
    Scanner::new(ruleset.spelling_rules, ruleset.case_rules)
}

// 4.2: Political / regional proper nouns

#[test]
fn country_laos() {
    let scanner = full_scanner();
    let issues = scanner.scan("老撾是東南亞國家").issues;
    assert_eq!(issues.len(), 1);
    assert_eq!(issues[0].found, "老撾");
    assert!(issues[0].suggestions.contains(&"寮國".to_string()));
}

#[test]
fn country_new_zealand() {
    let scanner = full_scanner();
    let issues = scanner.scan("他移民到新西蘭").issues;
    assert_eq!(issues.len(), 1);
    assert_eq!(issues[0].found, "新西蘭");
    assert!(issues[0].suggestions.contains(&"紐西蘭".to_string()));
}

#[test]
fn country_italy() {
    let scanner = full_scanner();
    let issues = scanner.scan("意大利的美食很有名").issues;
    assert_eq!(issues.len(), 1);
    assert_eq!(issues[0].found, "意大利");
    assert!(issues[0].suggestions.contains(&"義大利".to_string()));
}

#[test]
fn country_saudi() {
    let scanner = full_scanner();
    let issues = scanner.scan("沙特的石油產量很高").issues;
    assert_eq!(issues.len(), 1);
    assert_eq!(issues[0].found, "沙特");
    assert!(issues[0].suggestions.contains(&"沙烏地".to_string()));
}

#[test]
fn org_asean() {
    let scanner = full_scanner();
    let issues = scanner.scan("東盟峰會在曼谷舉行").issues;
    assert_eq!(issues.len(), 1);
    assert_eq!(issues[0].found, "東盟");
    assert!(issues[0].suggestions.contains(&"東協".to_string()));
    assert_eq!(issues[0].rule_type, IssueType::PoliticalColoring);
}

#[test]
fn org_commonwealth() {
    let scanner = full_scanner();
    let issues = scanner.scan("英聯邦有五十多個成員國").issues;
    assert_eq!(issues.len(), 1);
    assert_eq!(issues[0].found, "英聯邦");
    assert!(issues[0].suggestions.contains(&"大英國協".to_string()));
    assert_eq!(issues[0].rule_type, IssueType::PoliticalColoring);
}

#[test]
fn country_qatar() {
    let scanner = full_scanner();
    let issues = scanner.scan("卡塔爾世界杯在冬天舉行").issues;
    assert_eq!(issues.len(), 1);
    assert_eq!(issues[0].found, "卡塔爾");
    assert!(issues[0].suggestions.contains(&"卡達".to_string()));
}

#[test]
fn country_georgia() {
    let scanner = full_scanner();
    let issues = scanner.scan("格魯吉亞位於高加索地區").issues;
    assert_eq!(issues.len(), 1);
    assert_eq!(issues[0].found, "格魯吉亞");
    assert!(issues[0].suggestions.contains(&"喬治亞".to_string()));
}

#[test]
fn country_croatia() {
    let scanner = full_scanner();
    let issues = scanner.scan("克羅地亞的足球隊很強").issues;
    assert_eq!(issues.len(), 1);
    assert_eq!(issues[0].found, "克羅地亞");
    assert!(issues[0].suggestions.contains(&"克羅埃西亞".to_string()));
}

// Multiple political nouns in one sentence
#[test]
fn multiple_countries_in_prose() {
    let scanner = full_scanner();
    let issues = scanner.scan("意大利和新西蘭簽署了貿易協定").issues;
    assert_eq!(issues.len(), 2);
    let founds: Vec<&str> = issues.iter().map(|i| i.found.as_str()).collect();
    assert!(founds.contains(&"意大利"));
    assert!(founds.contains(&"新西蘭"));
}

// Clean text should not trigger
#[test]
fn tw_country_names_clean() {
    let scanner = full_scanner();
    let issues = scanner.scan("義大利和紐西蘭簽署了貿易協定").issues;
    // No political/country issues (might have other punctuation issues)
    let country_issues: Vec<_> = issues
        .iter()
        .filter(|i| i.found == "義大利" || i.found == "紐西蘭")
        .collect();
    assert!(country_issues.is_empty());
}

// 4.1: IT/software terminology

#[test]
fn it_probability() {
    let scanner = full_scanner();
    let issues = scanner.scan("這個事件的概率很低").issues;
    assert_eq!(issues.len(), 1);
    assert_eq!(issues[0].found, "概率");
    assert!(issues[0].suggestions.contains(&"機率".to_string()));
}

#[test]
fn it_probability_in_prose() {
    let scanner = full_scanner();
    let text = "根據貝氏定理計算後驗概率分布";
    let issues = scanner.scan(text).issues;
    let prob_issues: Vec<_> = issues.iter().filter(|i| i.found == "概率").collect();
    assert_eq!(prob_issues.len(), 1);
}

// Existing IT rules still work after expansion
#[test]
fn existing_it_rules_still_fire() {
    let scanner = full_scanner();
    let issues = scanner.scan("這個軟件需要更新").issues;
    assert!(issues.iter().any(|i| i.found == "軟件"));
}

#[test]
fn namespace_not_flagged() {
    // 命名空間 is already standard TW usage; the incorrect cross_strait rule
    // mapping it to 名稱空間/名字空間 was removed.
    let scanner = full_scanner();
    let issues = scanner.scan("使用命名空間隔離模組").issues;
    assert!(
        issues.iter().all(|i| i.found != "命名空間"),
        "命名空間 is correct TW and must NOT be flagged"
    );
}

#[test]
fn hackathon_keeps_heike() {
    // 駭客 is the zh-TW word for hacker in both senses (the cracker is 怪客),
    // so 黑客 still localizes to it. 黑客松 (hackathon) is the exception: that
    // loanword is what Taiwanese events call themselves, and 駭客松 is not.
    let scanner = full_scanner();
    let issues = scanner.scan("這位黑客寫出了編譯器").issues;
    assert!(issues.iter().any(|i| i.found == "黑客"), "{issues:?}");

    let issues = scanner.scan("總統盃黑客松的參賽團隊").issues;
    assert!(
        issues.iter().all(|i| i.found != "黑客"),
        "黑客松 must not be rewritten, got {:?}",
        issues.iter().map(|i| &i.found).collect::<Vec<_>>()
    );
}

#[test]
fn shenfen_card_is_a_strict_only_variant() {
    // 身份證 is the same glyph question as 身份, not a cross-strait term. As a
    // longer match it shadows the 身份 rule, so it has to carry the same type,
    // or strict mode reports one glyph choice at two different severities.
    let scanner = full_scanner();
    let issues = scanner.scan_profiled("身份證字號", Profile::Strict).issues;
    let hit = issues
        .iter()
        .find(|i| i.found == "身份證")
        .unwrap_or_else(|| panic!("{issues:?}"));
    assert_eq!(hit.rule_type, IssueType::Variant);

    let issues = scanner.scan_profiled("身份證字號", Profile::Base).issues;
    assert!(issues.iter().all(|i| i.found != "身份證"), "{issues:?}");
}

#[test]
fn row_column_vector_terms_not_swapped() {
    // 列/行 terms are valid Taiwanese terms in row/column contexts. Generic
    // math clues cannot prove the author meant the PRC sense, and swapping them
    // can reverse the mathematical meaning.
    let scanner = full_scanner();
    let issues = scanner
        .scan("矩陣的每一列可視為列向量；矩陣的每一行可視為行向量。初等列變換與初等行變換不同。")
        .issues;
    assert!(
        issues.iter().all(|i| {
            !matches!(
                i.found.as_str(),
                "列向量" | "行向量" | "初等列變換" | "初等行變換"
            )
        }),
        "row/column terms must not be swapped, got {:?}",
        issues.iter().map(|i| &i.found).collect::<Vec<_>>()
    );
}

#[test]
fn standard_composite_function_term_not_flagged() {
    // 合成函數 is standard mathematical usage in Taiwan; do not rewrite the
    // 合成 prefix to 複合 solely because 函數 is nearby.
    let scanner = full_scanner();
    let issues = scanner.scan("合成函數是函數之間的標準運算。").issues;
    assert!(
        issues.iter().all(|i| i.found != "合成"),
        "合成函數 must not be flagged, got {:?}",
        issues.iter().map(|i| &i.found).collect::<Vec<_>>()
    );
}

// Profile interaction: strict catches all + variants
#[test]
fn political_nouns_fire_under_all_profiles() {
    let scanner = full_scanner();
    for profile in Profile::ALL {
        let issues = scanner.scan_profiled("老撾是東南亞國家", *profile).issues;
        assert!(
            issues.iter().any(|i| i.found == "老撾"),
            "Profile {:?} should flag 老撾",
            profile
        );
    }
}

// 4.3: Context clues on ambiguous rules

#[test]
fn context_clues_present_on_ambiguous_rules() {
    let json_str = include_str!("../assets/ruleset.json");
    let ruleset: Ruleset = serde_json::from_str(json_str).unwrap();
    let ambiguous_terms = [
        // Existing 4
        "程序", "質量", "接口", "並行", // High-risk: common non-IT usage in Taiwan
        "函數", "分配", "刷新", "地址", "循環", "菜單", "證書",
        // Moderate-risk: domain-specific ambiguity
        "交互", "場景", "日誌", "嚮導", "語句", "社交",
        // Newly contextualized IT/UI terms must also be clue-gated
        "保存", "創建", "導航", "打開", "推遲", "搜索", "添加", "觸摸", "設置", "運行", "響應",
        "高級",
    ];
    for term in &ambiguous_terms {
        let rule = ruleset
            .spelling_rules
            .iter()
            .find(|r| r.from == *term && r.english.is_some())
            .unwrap_or_else(|| panic!("ambiguous rule for {} not found", term));
        assert!(
            rule.context_clues.is_some(),
            "Rule for {} should have context_clues",
            term
        );
        let clues = rule.context_clues.as_ref().unwrap();
        assert!(
            clues.len() >= 2,
            "Rule for {} should have at least 2 context clues, got {}",
            term,
            clues.len()
        );
    }
}

#[test]
fn context_clues_propagated_to_issue() {
    let scanner = full_scanner();
    let issues = scanner.scan("我需要編寫一個程序來執行").issues;
    let prog_issues: Vec<_> = issues.iter().filter(|i| i.found == "程序").collect();
    assert_eq!(prog_issues.len(), 1);
    assert!(
        prog_issues[0].context_clues.is_some(),
        "Issue for 程序 should carry context_clues"
    );
}

#[test]
fn fixer_lexical_safe_skips_context_clue_rules() {
    use zhtw_mcp::fixer::{apply_fixes, FixMode};

    let scanner = full_scanner();
    let text = "我需要編寫一個程序來執行";
    let issues = scanner.scan(text).issues;
    // LexicalSafe should skip 程序 because it has context_clues
    let result = apply_fixes(text, &issues, FixMode::LexicalSafe, &[]);
    assert!(
        result.text.contains("程序"),
        "LexicalSafe should not replace 程序 (has context_clues)"
    );
}

#[test]
fn fixer_lexical_contextual_with_segmenter_applies_when_clues_match() {
    use zhtw_mcp::engine::segment::Segmenter;
    use zhtw_mcp::fixer::{apply_fixes_with_context, FixMode};

    let json_str = include_str!("../assets/ruleset.json");
    let ruleset: Ruleset = serde_json::from_str(json_str).unwrap();
    let scanner = Scanner::new(ruleset.spelling_rules.clone(), ruleset.case_rules);
    let segmenter = Segmenter::from_rules(&ruleset.spelling_rules);

    let text = "我需要編寫一個程序來執行";
    let issues = scanner.scan(text).issues;
    let result = apply_fixes_with_context(
        text,
        &issues,
        FixMode::LexicalContextual,
        &[],
        Some(&segmenter),
    );
    assert!(
        result.text.contains("程式"),
        "LexicalContextual with segmenter should replace 程序->程式 when clues match"
    );
}

#[test]
fn fixer_lexical_contextual_with_segmenter_skips_when_no_clues() {
    use zhtw_mcp::engine::segment::Segmenter;
    use zhtw_mcp::fixer::{apply_fixes_with_context, FixMode};

    let json_str = include_str!("../assets/ruleset.json");
    let ruleset: Ruleset = serde_json::from_str(json_str).unwrap();
    let scanner = Scanner::new(ruleset.spelling_rules.clone(), ruleset.case_rules);
    let segmenter = Segmenter::from_rules(&ruleset.spelling_rules);

    let text = "這個程序很複雜";
    let issues = scanner.scan(text).issues;
    let result = apply_fixes_with_context(
        text,
        &issues,
        FixMode::LexicalContextual,
        &[],
        Some(&segmenter),
    );
    assert!(
        result.text.contains("程序"),
        "LexicalContextual should skip 程序 when context clues are insufficient"
    );
}

// English anchor present for disambiguation
#[test]
fn country_rules_have_english_field() {
    let json_str = include_str!("../assets/ruleset.json");
    let ruleset: Ruleset = serde_json::from_str(json_str).unwrap();
    let country_terms = ["老撾", "沙特", "新西蘭", "意大利", "卡塔爾", "格魯吉亞"];
    for term in &country_terms {
        let rule = ruleset
            .spelling_rules
            .iter()
            .find(|r| r.from == *term)
            .unwrap_or_else(|| panic!("rule for {} not found", term));
        assert!(
            rule.english.is_some(),
            "Rule for {} should have english field",
            term
        );
    }
}

// 6.2: Political stance profiles

#[test]
fn roc_centric_flags_all_political_terms() {
    let scanner = full_scanner();
    let cfg = Profile::Base.config();
    // RocCentric (default): 內地 should be flagged
    let excluded = vec![];
    let issues = scanner
        .scan_with_config("這是中國內地的情況", &excluded, cfg)
        .issues;
    assert!(
        issues.iter().any(|i| i.found == "內地"),
        "RocCentric should flag 內地"
    );
}

#[test]
fn roc_centric_flags_asean() {
    let scanner = full_scanner();
    let cfg = Profile::Base.config();
    let issues = scanner.scan_with_config("東盟峰會", &[], cfg).issues;
    assert!(
        issues.iter().any(|i| i.found == "東盟"),
        "RocCentric should flag 東盟"
    );
}

#[test]
fn international_skips_identity_terms() {
    let scanner = full_scanner();
    let cfg = Profile::Base
        .config()
        .with_stance(PoliticalStance::International);
    // International: 內地 should NOT be flagged (identity-loaded)
    let issues = scanner
        .scan_with_config("這是中國內地的情況", &[], cfg)
        .issues;
    assert!(
        !issues.iter().any(|i| i.found == "內地"),
        "International should NOT flag 內地"
    );
}

#[test]
fn international_keeps_org_names() {
    let scanner = full_scanner();
    let cfg = Profile::Base
        .config()
        .with_stance(PoliticalStance::International);
    // International: 東盟 should still be flagged (org name)
    let issues = scanner.scan_with_config("東盟峰會", &[], cfg).issues;
    assert!(
        issues.iter().any(|i| i.found == "東盟"),
        "International should still flag 東盟"
    );
}

#[test]
fn neutral_suppresses_all_political() {
    let scanner = full_scanner();
    let cfg = Profile::Base.config().with_stance(PoliticalStance::Neutral);
    // Neutral: neither 內地 nor 東盟 should be flagged
    let issues = scanner.scan_with_config("內地的東盟峰會", &[], cfg).issues;
    let political: Vec<_> = issues
        .iter()
        .filter(|i| i.rule_type == IssueType::PoliticalColoring)
        .collect();
    assert!(
        political.is_empty(),
        "Neutral should suppress all political_coloring rules, got {:?}",
        political.iter().map(|i| &i.found).collect::<Vec<_>>()
    );
}

#[test]
fn neutral_still_flags_cross_strait() {
    let scanner = full_scanner();
    let cfg = Profile::Base.config().with_stance(PoliticalStance::Neutral);
    // Neutral suppresses political but NOT cross_strait vocabulary
    let issues = scanner
        .scan_with_config("這個軟件需要更新", &[], cfg)
        .issues;
    assert!(
        issues.iter().any(|i| i.found == "軟件"),
        "Neutral should still flag cross_strait terms like 軟件"
    );
}

#[test]
fn stance_allows_rule_logic() {
    // Unit-level check for allows_rule
    assert!(PoliticalStance::RocCentric.allows_rule("內地"));
    assert!(PoliticalStance::RocCentric.allows_rule("東盟"));
    assert!(!PoliticalStance::International.allows_rule("內地"));
    assert!(!PoliticalStance::International.allows_rule("大陸同胞"));
    assert!(!PoliticalStance::International.allows_rule("祖國"));
    assert!(PoliticalStance::International.allows_rule("東盟"));
    assert!(PoliticalStance::International.allows_rule("英聯邦"));
    assert!(!PoliticalStance::Neutral.allows_rule("內地"));
    assert!(!PoliticalStance::Neutral.allows_rule("東盟"));
}

// Context-clue gate: scanner-level false-positive suppression

#[test]
fn scanner_suppresses_zhichi_in_political_context() {
    // 支持 in a political context (no IT context clues) must NOT fire.
    let scanner = full_scanner();
    let issues = scanner
        .scan("許多代理商擅自發表「支持統一」的言論，母公司並未反對。")
        .issues;
    assert!(
        issues.iter().all(|i| i.found != "支持"),
        "支持 must not fire in non-IT political context, got {:?}",
        issues.iter().map(|i| &i.found).collect::<Vec<_>>()
    );
}

#[test]
fn scanner_context_clues_do_not_cross_contaminate_matches() {
    let scanner = full_scanner();

    for (text, untouched) in [
        ("大型語言模型的上下文窗口為十萬個令牌", "窗口"),
        ("這篇論文探討恆星演化中的質量問題", "質量"),
    ] {
        let issues = scanner.scan(text).issues;
        assert!(issues.iter().all(|issue| issue.found != untouched));
    }

    let issues = scanner
        .scan("報告把中國臺灣列為區域，並說明中國臺灣與美國之間的關係")
        .issues;
    assert_eq!(
        issues
            .iter()
            .filter(|issue| issue.found == "中國臺灣")
            .count(),
        2
    );

    let issues = scanner.scan("日本學者訪問中國高校，討論大學合作").issues;
    assert!(issues.iter().any(|issue| issue.found == "中國高校"));

    // A pass-sense object later in the same clause wins over a via reading, so
    // only a clause without one stays flagged.
    for text in ["資料通過系統提交提案", "使用者通過 API 取得資料"] {
        let issues = scanner.scan(text).issues;
        assert!(issues.iter().any(|issue| issue.found == "通過"));
    }

    let text = "資料通過系統提交，提案也通過稽核";
    let issues = scanner.scan(text).issues;
    let matches: Vec<_> = issues
        .iter()
        .filter(|issue| issue.found == "通過")
        .collect();
    assert_eq!(matches.len(), 1, "{issues:?}");
    assert_eq!(matches[0].offset, text.find("通過").unwrap());

    // The pass-sense object is a clause-bounded positional veto, so any
    // modifier between 通過 and it is covered without being listed, and a
    // via-sense occurrence in another clause is not suppressed.
    for text in [
        "立法院通過了新的法案，建立新的機制。",
        "董事會通過了下半年預算，也更新演算法。",
        "他通過了期末考，用新方法準備。",
        "軟體通過所有測試，系統已上線。",
        "設備通過安全檢驗，並透過網路回報。",
        "版本通過嚴格審核，再透過 API 發佈。",
        "設備通過工安檢驗，並透過網路回報。",
        "軟體通過全部測試，系統已上線。",
        "版本通過嚴密審核，再透過 API 發佈。",
        "系統通過壓力測試。",
        "他通過專業認證。系統稍後更新。",
    ] {
        let issues = scanner.scan(text).issues;
        assert!(
            issues.iter().all(|issue| issue.found != "通過"),
            "{text}: {issues:?}"
        );
    }

    // Pass-sense objects beyond tests and reviews, the noun-first order, and
    // the rate noun all keep the pass sense.
    for text in [
        "這套系統通過了驗證。",
        "新版 API 通過檢查後才上線。",
        "系統通過評估後正式啟用。",
        "本系統通過資安檢測。",
        "這個方法可以讓程式碼通過編譯。",
        "修改後系統才能通過 CI。",
        "系統的通過率很高。",
        "系統的驗證通過後上線。",
        "他順利通過。系統稍後更新。",
    ] {
        let issues = scanner.scan(text).issues;
        assert!(
            issues.iter().all(|issue| issue.found != "通過"),
            "{text}: {issues:?}"
        );
    }

    // 驗證 and 檢查 are only exempt next to 通過: as clause-wide vetoes they
    // also swallowed the via sense in 通過鏈接檢查器驗證, where 檢查器 is the
    // channel. A modifier in between (通過安全驗證) is a known false positive.
    let issues = scanner.scan("軟件更新會通過鏈接檢查器驗證文檔引用").issues;
    assert!(
        issues.iter().any(|issue| issue.found == "通過"),
        "{issues:?}"
    );

    // Via-sense channels are clues, and a pass-sense veto in the next clause
    // does not reach back across the comma.
    for text in [
        "通過電子郵件傳送報告。",
        "通過代理伺服器連線。",
        "通過資料庫同步設定。",
        "使用者通過 API 取得資料，董事會稍後開會。",
    ] {
        let issues = scanner.scan(text).issues;
        assert!(
            issues.iter().any(|issue| issue.found == "通過"),
            "{text}: {issues:?}"
        );
    }

    // Juxtaposed place names are a list, not the political name.
    for text in [
        "中國臺灣之間的貿易逐年成長。",
        "中國台灣香港三地的學者。",
        "中國台灣日本韓國都參加。",
    ] {
        let issues = scanner.scan(text).issues;
        assert!(
            issues
                .iter()
                .all(|issue| issue.rule_type != IssueType::PoliticalColoring),
            "{text}: {issues:?}"
        );
    }

    for term in ["產品質量", "商品質量"] {
        let issues = scanner.scan(&format!("{term}不佳")).issues;
        assert!(issues.iter().any(|issue| issue.found == term));
    }

    // The mass guard reads the same clause only: the weight after the comma
    // belongs to 重量, so the quality sense before it still fires.
    for text in [
        "產品質量很好，重量為兩公斤。",
        "產品質量很好, 重量為兩公斤。",
        "產品質量很好. 重量為兩公斤。",
    ] {
        let issues = scanner.scan(text).issues;
        assert!(
            issues.iter().any(|issue| issue.found.contains("質量")),
            "{text}: {issues:?}"
        );
    }

    for text in [
        "產品質量為 1,000 公斤。",
        "產品質量為 2.5 公斤。",
        "商品質量為一公噸。",
        "產品質量為 3 KG。",
        "兩公斤的產品質量",
        "產品質量為五百公克。",
        "產品質量為 5kg。",
        "商品質量為兩噸。",
        "產品的質量約 2 噸。",
    ] {
        let issues = scanner.scan(text).issues;
        assert!(
            issues.iter().all(|issue| !issue.found.contains("質量")),
            "{text}: {issues:?}"
        );
    }

    // The unit guard names 公克/毫克/千克, not a bare 克, which sits inside
    // 巧克力 and 克服 far more often than it stands alone as a unit. The price
    // is that 產品質量為五百克 reads as quality. An ASCII unit or pass-sense
    // object is a whole word, so pkg and ASCII do not trip kg and CI.
    for (text, term) in [
        ("這款巧克力的產品質量很好。", "產品質量"),
        ("團隊克服困難，產品質量大幅提升。", "產品質量"),
        ("pkg 產品質量管理流程", "產品質量"),
        ("資料通過 ASCII 網路系統傳送。", "通過"),
    ] {
        let issues = scanner.scan(text).issues;
        assert!(
            issues.iter().any(|issue| issue.found == term),
            "{text}: {issues:?}"
        );
    }
    let issues = scanner.scan("修改後系統才能通過 CI 流程。").issues;
    assert!(
        issues.iter().all(|issue| issue.found != "通過"),
        "{issues:?}"
    );

    for text in ["癌症資料篩查可找出異常紀錄", "癌症數據篩查可找出異常紀錄"]
    {
        let issues = scanner.scan(text).issues;
        assert!(
            issues.iter().all(|issue| issue.found != "篩查"),
            "{text}: {issues:?}"
        );
    }

    // A clue in another sentence is not context for this one.
    let issues = scanner.scan("癌症資料另存。系統正在篩查紀錄。").issues;
    assert!(
        issues.iter().all(|issue| issue.found != "篩查"),
        "{issues:?}"
    );

    for text in ["醫院提供視力篩查服務。", "產前篩查與基因篩查"] {
        let issues = scanner.scan(text).issues;
        assert!(
            issues.iter().any(|issue| issue.found == "篩查"),
            "{text}: {issues:?}"
        );
    }

    let issues = scanner.scan("資料已備妥, 癌症篩查明日開始").issues;
    assert!(
        issues.iter().any(|issue| issue.found == "篩查"),
        "{issues:?}"
    );

    let issues = scanner.scan("這則消息，來源尚未確認。").issues;
    assert!(
        issues.iter().all(|issue| issue.found != "消息"),
        "{issues:?}"
    );

    let issues = scanner.scan("The anthropic principle is debated.").issues;
    assert!(issues.iter().all(|issue| issue.found != "anthropic"));
}

#[test]
fn scanner_prefers_the_longer_form_of_a_suffixed_term() {
    // A rule whose "to" already carries the suffix the source term repeats
    // writes nonsense when the short rule wins: 老年痴呆 -> 失智症 applied to
    // 老年痴呆症 yields 失智症症, and 請求頭 -> 請求標頭 applied to 請求頭部
    // yields 請求標頭部. The longer rule has to exist so overlap resolution
    // consumes the suffix too.
    let scanner = full_scanner();

    for (text, want_found, want_to) in [
        ("老年痴呆症的診斷標準已更新", "老年痴呆症", "失智症"),
        ("請求頭部欄位需要調整", "請求頭部", "請求標頭"),
        ("老年癡呆症的診斷標準已更新", "老年癡呆症", "失智症"),
    ] {
        let issues = scanner.scan(text).issues;
        let hit = issues
            .iter()
            .find(|issue| issue.found == want_found)
            .unwrap_or_else(|| {
                panic!(
                    "{text}: expected {want_found}, got {:?}",
                    issues.iter().map(|i| &i.found).collect::<Vec<_>>()
                )
            });
        assert!(
            hit.suggestions.iter().any(|s| s == want_to),
            "{text}: expected {want_to}, got {:?}",
            hit.suggestions
        );
    }
}

#[test]
fn scanner_fires_zhichi_in_it_context() {
    // 支持 next to IT context clues (瀏覽器) must fire.
    let scanner = full_scanner();
    let issues = scanner.scan("此瀏覽器支持 WebGL 渲染。").issues;
    assert!(
        issues.iter().any(|i| i.found == "支持"),
        "支持 must fire when IT context clue 瀏覽器 is nearby"
    );
}

#[test]
fn scanner_suppresses_shengming_in_political_context() {
    // 聲明 as a public statement (no programming context clues) must NOT fire.
    let scanner = full_scanner();
    let issues = scanner
        .scan("除非母公司曾發表聲明反對代理商的言論，否則視兩者為同一立場。")
        .issues;
    assert!(
        issues.iter().all(|i| i.found != "聲明"),
        "聲明 must not fire in non-programming context, got {:?}",
        issues.iter().map(|i| &i.found).collect::<Vec<_>>()
    );
}

#[test]
fn scanner_fires_shengming_in_programming_context() {
    // 聲明 next to a programming context clue (變數) must fire.
    let scanner = full_scanner();
    let issues = scanner.scan("在函式開頭的變數聲明需要明確型別。").issues;
    assert!(
        issues.iter().any(|i| i.found == "聲明"),
        "聲明 must fire when programming context clue 變數 is nearby"
    );
}

#[test]
fn scanner_suppresses_baocun_near_negative_clue() {
    // negative_context_clues: 食物, 文化, 遺產, 期限
    let scanner = full_scanner();
    let issues = scanner.scan("食物保存期限應標示清楚").issues;
    assert!(
        issues.iter().all(|i| i.found != "保存"),
        "保存 must not fire when negative clue '食物'/'期限' is nearby, got {:?}",
        issues.iter().map(|i| &i.found).collect::<Vec<_>>()
    );
}

#[test]
fn scanner_suppresses_baocun_in_general_prose() {
    let scanner = full_scanner();
    let issues = scanner.scan("警方保存證據以供調查").issues;
    assert!(
        issues.iter().all(|i| i.found != "保存"),
        "保存 must not fire without nearby IT clues, got {:?}",
        issues.iter().map(|i| &i.found).collect::<Vec<_>>()
    );
}

#[test]
fn scanner_fires_baocun_in_it_context() {
    let scanner = full_scanner();
    let issues = scanner.scan("請按下保存按鈕以保存檔案").issues;
    assert!(
        issues.iter().any(|i| i.found == "保存"),
        "保存 must fire when IT clues are nearby"
    );
}

#[test]
fn scanner_suppresses_daohang_near_negative_clue() {
    // negative_context_clues: 衛星, GPS, 汽車, 路線
    let scanner = full_scanner();
    let issues = scanner.scan("汽車導航系統使用衛星定位").issues;
    assert!(
        issues.iter().all(|i| i.found != "導航"),
        "導航 must not fire when negative clue '汽車'/'衛星' is nearby, got {:?}",
        issues.iter().map(|i| &i.found).collect::<Vec<_>>()
    );
}

#[test]
fn scanner_suppresses_daohang_in_general_prose() {
    let scanner = full_scanner();
    let issues = scanner.scan("這本書提供職涯導航與建議").issues;
    assert!(
        issues.iter().all(|i| i.found != "導航"),
        "導航 must not fire without nearby UI clues, got {:?}",
        issues.iter().map(|i| &i.found).collect::<Vec<_>>()
    );
}

#[test]
fn scanner_fires_daohang_in_ui_context() {
    let scanner = full_scanner();
    let issues = scanner.scan("網站導航選單需要重新設計").issues;
    assert!(
        issues.iter().any(|i| i.found == "導航"),
        "導航 must fire when UI clues are nearby"
    );
}

#[test]
fn scanner_suppresses_yunxing_near_negative_clue() {
    // negative_context_clues: 軌道, 列車, 班次
    let scanner = full_scanner();
    let issues = scanner.scan("列車運行時刻表已更新").issues;
    assert!(
        issues.iter().all(|i| i.found != "運行"),
        "運行 must not fire when negative clue '列車' is nearby, got {:?}",
        issues.iter().map(|i| &i.found).collect::<Vec<_>>()
    );
}

#[test]
fn scanner_suppresses_yunxing_in_general_prose() {
    let scanner = full_scanner();
    let issues = scanner.scan("這項計畫運行順利").issues;
    assert!(
        issues.iter().all(|i| i.found != "運行"),
        "運行 must not fire without nearby software clues, got {:?}",
        issues.iter().map(|i| &i.found).collect::<Vec<_>>()
    );
}

#[test]
fn scanner_fires_yunxing_in_software_context() {
    let scanner = full_scanner();
    let issues = scanner.scan("系統運行腳本後會重新啟動服務").issues;
    assert!(
        issues.iter().any(|i| i.found == "運行"),
        "運行 must fire when software clues are nearby"
    );
}

// 14.4: CS Terminology, 參數 must NOT be flagged (correct zh-TW for parameter)

#[test]
fn parameter_canshu_not_flagged() {
    // 參數 is the correct zh-TW term for "parameter"; the old rule incorrectly
    // flagged it as wrong. After disabling that rule, it must not fire.
    let scanner = full_scanner();
    let issues = scanner.scan("函式的參數需要明確型別").issues;
    assert!(
        issues.iter().all(|i| i.found != "參數"),
        "參數 is correct zh-TW for 'parameter' and must NOT be flagged, got: {:?}",
        issues
            .iter()
            .filter(|i| i.found == "參數")
            .collect::<Vec<_>>()
    );
}

#[test]
fn argument_yinshu_not_affected() {
    // 實參 (CN for "argument") should still be flagged → 引數
    let scanner = full_scanner();
    let issues = scanner.scan("呼叫函式時的實參需要符合型別").issues;
    assert!(
        issues.iter().any(|i| i.found == "實參"),
        "實參 (CN term for 'argument') should still be flagged"
    );
}

// 宏 rule: exceptions for compound words where 宏 means "grand/vast"

#[test]
fn macro_hong_fires_standalone() {
    let scanner = full_scanner();
    // 宏 rule is clue-gated; needs a macro-related clue nearby.
    let issues = scanner.scan("這個宏是用 #define 展開的").issues;
    assert!(
        issues.iter().any(|i| i.found == "宏"),
        "standalone 宏 (macro) must be flagged when macro clues present"
    );
}

#[test]
fn macro_hong_requires_macro_clue() {
    let scanner = full_scanner();
    let issues = scanner.scan("這個宏定義了一個函式").issues;
    assert!(
        issues.iter().all(|i| i.found != "宏"),
        "宏 should stay suppressed without configured macro clues"
    );
}

#[test]
fn macro_hong_skips_hongguan() {
    // 宏觀 = macroscopic: NOT a programming macro
    let scanner = full_scanner();
    let issues = scanner.scan("宏觀經濟學是重要的學科").issues;
    assert!(
        issues.iter().all(|i| i.found != "宏"),
        "宏觀 must not trigger 宏→巨集 rule, got: {:?}",
        issues
            .iter()
            .filter(|i| i.found == "宏")
            .collect::<Vec<_>>()
    );
}

#[test]
fn macro_hong_skips_hongwei() {
    // 宏偉 = grand/imposing: NOT a programming macro
    let scanner = full_scanner();
    let issues = scanner.scan("這是一座宏偉的建築").issues;
    assert!(
        issues.iter().all(|i| i.found != "宏"),
        "宏偉 must not trigger 宏→巨集 rule, got: {:?}",
        issues
            .iter()
            .filter(|i| i.found == "宏")
            .collect::<Vec<_>>()
    );
}

#[test]
fn macro_hong_skips_huihong() {
    // 恢宏 = magnificent: 宏 at position 1, not position 0
    let scanner = full_scanner();
    let issues = scanner.scan("氣勢恢宏的場面").issues;
    assert!(
        issues.iter().all(|i| i.found != "宏"),
        "恢宏 must not trigger 宏→巨集 rule, got: {:?}",
        issues
            .iter()
            .filter(|i| i.found == "宏")
            .collect::<Vec<_>>()
    );
}

#[test]
fn macro_hong_skips_hongqi() {
    // 宏碁 = Acer (Taiwan brand name)
    let scanner = full_scanner();
    let issues = scanner.scan("宏碁電腦是台灣品牌").issues;
    assert!(
        issues.iter().all(|i| i.found != "宏"),
        "宏碁 must not trigger 宏→巨集 rule, got: {:?}",
        issues
            .iter()
            .filter(|i| i.found == "宏")
            .collect::<Vec<_>>()
    );
}

#[test]
fn monolithic_kernel_suggestion() {
    // 宏內核 should suggest 單體式核心, not 單核心
    let scanner = full_scanner();
    let issues = scanner.scan("Linux 是宏內核架構").issues;
    let hit = issues.iter().find(|i| i.found == "宏內核");
    assert!(hit.is_some(), "宏內核 must be flagged");
    assert!(
        hit.unwrap().suggestions.contains(&"單體式核心".to_string()),
        "宏內核 should suggest 單體式核心, got: {:?}",
        hit.unwrap().suggestions
    );
}

// Geospatial / military / networking terminology
//
// Target terms for 座標, 麥卡托, 格網, 停駐, 生命週期, 下拉式選單 and 相符 come
// from externals/wintak-taiwan-coordinates, a WinTAK mapping plugin whose zh-TW
// documentation is gated on this linter. The rest follow 國家教育研究院 樂詞網
// and 內政部國土測繪中心 usage, as recorded in each rule's context field.
//
// Every rule here is two to four characters, so the aho-corasick matcher can
// hit across morpheme boundaries (方差 inside 地方差異, 慣導 inside 習慣導致).
// Each rule therefore gets a boundary-crossing negative case, and each gated
// rule gets a negative that carries a positive clue, so the test fails if the
// exception or negative clue is deleted.

fn shared_scanner() -> &'static Scanner {
    static SCANNER: std::sync::OnceLock<Scanner> = std::sync::OnceLock::new();
    SCANNER.get_or_init(full_scanner)
}

/// Assert that scanning text flags found exactly once, with suggestion first.
fn assert_flags(text: &str, found: &str, suggestion: &str) {
    let issues = shared_scanner().scan(text).issues;
    let hits: Vec<_> = issues.iter().filter(|i| i.found == found).collect();
    assert_eq!(
        hits.len(),
        1,
        "{} must be flagged exactly once in {:?}, got {:?}",
        found,
        text,
        issues
    );
    assert_eq!(
        hits[0].suggestions.first().map(String::as_str),
        Some(suggestion),
        "{} should suggest {} first, got: {:?}",
        found,
        suggestion,
        hits[0].suggestions
    );
}

/// Assert that scanning text does not flag found.
fn assert_clean(text: &str, found: &str) {
    let issues = shared_scanner().scan(text).issues;
    assert!(
        issues.iter().all(|i| i.found != found),
        "{} must not be flagged in {:?}, got {:?}",
        found,
        text,
        issues
    );
}

#[test]
fn geo_coordinate_term() {
    assert_flags("請輸入坐標後按下前往", "坐標", "座標");
    assert_flags("使用通用橫墨卡托投影", "墨卡托", "麥卡托");
    assert_flags("這份遙感影像的解析度不足", "遙感", "遙測");

    // 坐標系 must still win by leftmost-longest, and must fire in a geodetic
    // context rather than only a mathematical one.
    assert_flags("TWD97 坐標系統的轉換參數", "坐標系", "座標系統");
    assert_clean("TWD97 坐標系統的轉換參數", "坐標");
}

#[test]
fn geo_coordinate_boundary_crossings() {
    // 坐 is a common verb and 標 opens many words, so the pattern spans a
    // morpheme boundary in ordinary prose. Every one of these was a
    // reproducible false positive, and under safe-fix it rewrote the text.
    for text in [
        "請坐標準姿勢以免脊椎受傷",
        "他坐標示的位置等候",
        "遊客乘坐標高三千公尺的纜車，輸入資料後送出",
        "抗議民眾靜坐標語寫著訴求，經緯度不明",
        "他打坐標榜自己的定位與經度",
    ] {
        assert_clean(text, "坐標");
    }
    assert_clean("他有一種逍遙感覺", "遙感");
    assert_clean("遙感應器材已經安裝完成", "遙感");
    assert_clean("遙感探測是中央大學的研究領域", "遙感");
}

#[test]
fn geo_coordinate_survives_common_prefixes() {
    // The words guarding the boundary above must not swallow the real hits:
    // 針對, 終端 and 範圍 all end in a character that starts a 坐 verb.
    assert_flags("針對坐標轉換的精度做測試", "坐標", "座標");
    assert_flags("終端坐標的轉換需要投影參數", "坐標", "座標");
    assert_flags("範圍坐標的經緯度換算", "坐標", "座標");
}

#[test]
fn new_rules_skip_their_boundary_collisions() {
    assert_clean("現場的聒噪聲音持續不斷", "噪聲");
    assert_clean("航空通訊系統每半小時傳輸一次氣象報文", "報文");
    assert_clean("在地圖上畫出網格以便定位", "網格");
    assert_clean("視窗前的乘客看著飛機停靠空橋", "停靠");
    assert_clean("側邊的視窗外，遊艇停靠在防波堤旁", "停靠");

    // Each of these splits the rule across a word boundary: 不停|靠近,
    // 馬匹|配種, 平方|差, 督導|彈藥, 官網|格式, 海報|文案, 分組|播送,
    // 遮掩|碼頭. The left word carries the boundary, so it lives in the
    // segmenter lexicon rather than in a per-rule exception.
    assert_clean("游標不停靠近側邊面板時，工具列就會浮動出現", "停靠");
    assert_clean("牧場的馬匹配種紀錄要跟血統書逐筆比對", "匹配");
    assert_clean("國小數學先教平方差公式，統計課才會用到標準差", "方差");
    assert_clean("長官督導彈藥庫存，並檢查部隊武器保養狀況", "導彈");
    assert_clean("報名表請照官網格式填寫，並附上住家經緯度座標", "網格");
    assert_clean("這張海報文案要重新排版，再上傳到伺服器", "報文");
    assert_clean("電視台把節目分組播送到不同頻道的網路串流平台", "組播");
    assert_clean("工人用帆布遮掩碼頭的貨櫃，網路攝影機拍不到", "掩碼");
    // 被丟包 is the colloquial "stood up", a homograph rather than a boundary.
    assert_clean("網路上流傳很多被丟包的影片，真的很誇張", "丟包");
    assert_clean("官方文件提供了完整的 C++ 範例程式碼供開發者參考。", "例程");
}

#[test]
fn the_two_network_rules_do_not_cancel_each_other() {
    // 組播報文 once produced nothing at all: an exception on 組播 and a lexicon
    // entry for 播報 combined into a silent double miss on the sentence both
    // rules exist for. 組播 fires again, so the sentence is caught.
    assert_flags("路由器會把組播報文轉送到各個網段", "組播", "群播");

    // 報文 stays masked here, and that is the intended trade rather than a
    // leftover of the double miss. 播報 is in the segmenter lexicon because it
    // guards two real false positives, and it also covers the 報 of 報文
    // whenever a 播 precedes it. Both guards below are what pay for that.
    assert_clean("路由器會把組播報文轉送到各個網段", "報文");
    assert_clean("主播播報文稿的速度很快", "報文");
    assert_clean("新聞組播報今天的頭條", "組播");
}

#[test]
fn geo_grid_needs_surveying_context() {
    // Surveying grid is 格網 in zh-TW.
    assert_flags("軍事網格座標以圖幅編號標示", "網格", "格網");

    // Mesh, layout and compute grids keep 網格 even with a surveying clue
    // nearby, which is what the negative clues and exceptions are for.
    assert_clean("地圖模型的網格貼圖需要重新算圖", "網格");
    assert_clean("高程模型的網格劃分交給有限元素分析", "網格");
    assert_clean("這個版面用三欄網格排版", "網格");
}

#[test]
fn military_terms() {
    assert_flags("飛彈射程內的導彈威脅", "導彈", "飛彈");
    assert_flags("精確導引武器採用雷射制導", "制導", "導引");
    assert_flags("衛星訊號中斷時由慣導維持定位", "慣導", "慣性導航");
}

#[test]
fn military_boundary_crossings() {
    // 輔導/領導 + 彈性, 抑制/強制 + 導入, 習慣 + 導致 are ordinary zh-TW.
    assert_clean("飛彈部隊也推動輔導彈性學習", "導彈");
    assert_clean("領導彈性調整值勤時間", "導彈");
    assert_clean("雷射加工要抑制導電性", "制導");
    assert_clean("武器庫存管理強制導入新流程", "制導");
    assert_clean("導航習慣導致他忽略警示", "慣導");
}

#[test]
fn network_terms() {
    assert_flags("以組播方式在網路上散布位置回報", "組播", "群播");
    assert_flags("鏈路品質不佳時會丟包", "丟包", "封包遺失");
    assert_flags("解析封包標頭的報文欄位", "報文", "訊息");
    assert_flags("設定網段的掩碼後重新連線", "掩碼", "遮罩");
    // The longer 子網掩碼 rule wins by leftmost-longest.
    assert_flags("子網掩碼設定錯誤", "子網掩碼", "子網路遮罩");
    assert_clean("子網掩碼設定錯誤", "掩碼");
}

#[test]
fn network_boundary_crossings() {
    assert_clean("網路節目組播出的時段", "組播");
    assert_clean("上傳封包前先丟包裹到郵局", "丟包");
    assert_clean("解析欄位時附上情報文件", "報文");
    assert_clean("傳輸協定的通報文號已建檔", "報文");
}

#[test]
fn statistics_terms() {
    assert_flags("取樣結果的均值與標準差", "均值", "平均值");
    assert_flags("統計樣本的方差偏高", "方差", "變異數");
}

#[test]
fn statistics_boundary_crossings() {
    assert_clean("統計上兩者均值得參考", "均值");
    assert_clean("取樣時的均值定理推導", "均值");
    assert_clean("統計顯示地方差異很大", "方差");
    assert_clean("估計借方差額後沖銷", "方差");
    // The correct term must survive the substring match.
    assert_clean("樣本的平均值符合預期", "均值");
}

#[test]
fn signal_and_ui_terms() {
    assert_flags("無線電的噪聲蓋過通聯", "噪聲", "雜訊");
    assert_flags("從下拉框選擇縣市", "下拉框", "下拉式選單");
    assert_flags("外掛的生命周期由主程式管理", "生命周期", "生命週期");
    assert_clean("鼓噪聲響持續了整個下午", "噪聲");
}

#[test]
fn dock_needs_window_context() {
    assert_flags("把窗格停靠在視窗右側", "停靠", "停駐");
    // Positive clue present, negative clue and exception must still win.
    assert_clean("視窗外就是公車停靠站", "停靠");
    assert_clean("面板上顯示船隻停靠的港口", "停靠");
}

#[test]
fn match_needs_software_context() {
    assert_flags("字串匹配失敗時回傳空結果", "匹配", "相符");
    // Positive clue present, exception must still win.
    assert_clean("搜尋欄位旁的天線阻抗匹配要調整", "匹配");
}

#[test]
fn digital_gate_stays_narrow() {
    // 數字 meaning "number" is far more common than 數字 meaning "digital", so
    // the gate must not widen to mapping or imaging vocabulary.
    assert_flags("數字轉型的技術藍圖", "數字", "數位");
    assert_clean("地圖上的數字代表路線編號", "數字");
    assert_clean("高程欄位的數字要對齊", "數字");
    assert_clean("訊號強度的數字是負七十", "數字");
}

#[test]
fn stack_top_terms() {
    assert_flags(
        "若當前堆疊非空，則棧頂函式在此刻被中斷。",
        "棧頂",
        "堆疊頂端",
    );
    assert_clean("程式執行時棧頂指標被修改", "棧");
    assert_flags("資料結構中的棧採用後進先出原則", "棧", "堆疊");
}
