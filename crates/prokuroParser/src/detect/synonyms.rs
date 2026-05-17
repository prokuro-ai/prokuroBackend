use serde::Deserialize;

#[derive(Deserialize)]
struct SynonymsFile {
    group: Vec<SynonymGroup>,
}

#[derive(Deserialize)]
struct SynonymGroup {
    aliases: Vec<String>,
}

pub fn default_synonyms() -> Vec<Vec<String>> {
    vec![
        vec![
            "mpn", "mfr part #", "mfg part #", "mfr. part #", "manf. part #",
            "manufacturer part number", "manufacturer part", "manufacturer pn",
            "part number", "part no", "part #", "pn", "manufacturer sku",
        ],
        vec!["qty", "quantity", "count", "amount", "q", "number", "number of", "total"],
        vec![
            "reference", "ref", "refdes", "designator", "refs", "references",
            "ref designator", "component reference",
        ],
        vec!["manufacturer", "mfr", "mfg", "brand", "make", "mfgr"],
        vec![
            "description", "desc", "value", "designation", "device",
            "component", "part description", "specification",
        ],
        vec!["footprint", "package", "case", "pcb footprint", "pcb package", "pattern"],
    ]
    .into_iter()
    .map(|group| group.into_iter().map(String::from).collect())
    .collect()
}

pub fn load_synonyms(path: &std::path::Path) -> Vec<Vec<String>> {
    let Ok(content) = std::fs::read_to_string(path) else {
        return default_synonyms();
    };
    let Ok(file) = toml::from_str::<SynonymsFile>(&content) else {
        return default_synonyms();
    };
    file.group.into_iter().map(|g| g.aliases).collect()
}
