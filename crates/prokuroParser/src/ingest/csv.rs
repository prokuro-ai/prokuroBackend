use std::io::Cursor;

use super::ParseError;

const UTF8_BOM: &[u8] = b"\xEF\xBB\xBF";

pub fn read_csv(bytes: &[u8]) -> Result<Vec<Vec<String>>, ParseError> {
    let bytes = strip_utf8_bom(bytes);
    let delimiter = sniff_delimiter(bytes);

    let mut reader = ::csv::ReaderBuilder::new()
        .has_headers(false)
        .flexible(true)
        .delimiter(delimiter)
        .from_reader(Cursor::new(bytes));

    let mut grid = Vec::new();
    for row in reader.records() {
        let record = row?;
        grid.push(record.iter().map(ToOwned::to_owned).collect());
    }

    Ok(grid)
}

fn strip_utf8_bom(bytes: &[u8]) -> &[u8] {
    if bytes.starts_with(UTF8_BOM) { &bytes[UTF8_BOM.len()..] } else { bytes }
}

fn sniff_delimiter(bytes: &[u8]) -> u8 {
    let first_line = bytes
        .split(|byte| *byte == b'\n' || *byte == b'\r')
        .next()
        .unwrap_or_default();

    let mut comma_count = 0usize;
    let mut semicolon_count = 0usize;
    let mut tab_count = 0usize;

    for byte in first_line {
        match byte {
            b',' => comma_count += 1,
            b';' => semicolon_count += 1,
            b'\t' => tab_count += 1,
            _ => {}
        }
    }

    if semicolon_count > comma_count && semicolon_count >= tab_count {
        b';'
    } else if tab_count > comma_count && tab_count > semicolon_count {
        b'\t'
    } else {
        b','
    }
}

#[cfg(test)]
mod tests {
    use super::read_csv;

    #[test]
    fn strips_utf8_bom_prefix() {
        let input = b"\xEF\xBB\xBFmpn,qty\nABC-123,2\n";
        let grid = read_csv(input).expect("csv with utf-8 bom should parse");

        assert_eq!(grid[0], vec!["mpn".to_string(), "qty".to_string()]);
        assert_eq!(grid[1], vec!["ABC-123".to_string(), "2".to_string()]);
    }

    #[test]
    fn sniffs_comma_delimiter() {
        let input = b"mpn,qty,desc\nABC-123,2,resistor\n";
        let grid = read_csv(input).expect("comma-delimited csv should parse");

        assert_eq!(grid[0].len(), 3);
        assert_eq!(grid[1][2], "resistor");
    }

    #[test]
    fn sniffs_semicolon_delimiter() {
        let input = b"mpn;qty;desc\nABC-123;2;resistor\n";
        let grid = read_csv(input).expect("semicolon-delimited csv should parse");

        assert_eq!(grid[0].len(), 3);
        assert_eq!(grid[1][2], "resistor");
    }

    #[test]
    fn sniffs_tab_delimiter() {
        let input = b"mpn\tqty\tdesc\nABC-123\t2\tresistor\n";
        let grid = read_csv(input).expect("tab-delimited csv should parse");

        assert_eq!(grid[0].len(), 3);
        assert_eq!(grid[1][2], "resistor");
    }

    #[test]
    fn parses_quoted_fields_with_embedded_comma() {
        let input = b"mpn,desc\nABC-123,\"resistor, 1%\"\n";
        let grid = read_csv(input).expect("quoted field with comma should parse");

        assert_eq!(grid[1], vec!["ABC-123".to_string(), "resistor, 1%".to_string()]);
    }
}
