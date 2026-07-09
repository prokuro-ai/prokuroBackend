use std::io::Cursor;

use encoding_rs::{UTF_16BE, UTF_16LE, WINDOWS_1252};

use super::ParseError;

const UTF8_BOM: &[u8] = b"\xEF\xBB\xBF";
const UTF16LE_BOM: &[u8] = b"\xFF\xFE";
const UTF16BE_BOM: &[u8] = b"\xFE\xFF";
const UNSUPPORTED_ENCODING_MESSAGE: &str =
    "File encoding not supported. Please save your BOM as UTF-8 or Excel format.";

pub fn read_csv(bytes: &[u8]) -> Result<Vec<Vec<String>>, ParseError> {
    let decoded = decode_csv_bytes(bytes)?;
    let delimiter = sniff_delimiter(decoded.as_bytes());

    let mut reader = ::csv::ReaderBuilder::new()
        .has_headers(false)
        .flexible(true)
        .delimiter(delimiter)
        .from_reader(Cursor::new(decoded.into_bytes()));

    let mut grid = Vec::new();
    for row in reader.records() {
        let record = row?;
        grid.push(record.iter().map(ToOwned::to_owned).collect());
    }

    Ok(grid)
}

fn strip_utf8_bom(bytes: &[u8]) -> &[u8] {
    if bytes.starts_with(UTF8_BOM) {
        &bytes[UTF8_BOM.len()..]
    } else {
        bytes
    }
}

fn decode_csv_bytes(bytes: &[u8]) -> Result<String, ParseError> {
    if bytes.starts_with(UTF16LE_BOM) {
        return decode_utf16(&bytes[UTF16LE_BOM.len()..], UTF_16LE);
    }
    if bytes.starts_with(UTF16BE_BOM) {
        return decode_utf16(&bytes[UTF16BE_BOM.len()..], UTF_16BE);
    }

    let utf8_bytes = strip_utf8_bom(bytes);
    if let Ok(as_utf8) = std::str::from_utf8(utf8_bytes) {
        return Ok(as_utf8.to_string());
    }

    let (decoded, _, had_errors) = WINDOWS_1252.decode(utf8_bytes);
    if had_errors {
        return Err(ParseError::EncodingError(
            UNSUPPORTED_ENCODING_MESSAGE.to_string(),
        ));
    }
    Ok(decoded.into_owned())
}

fn decode_utf16(
    bytes: &[u8],
    encoding: &'static encoding_rs::Encoding,
) -> Result<String, ParseError> {
    let (decoded, _, had_errors) = encoding.decode(bytes);
    if had_errors {
        return Err(ParseError::EncodingError(
            UNSUPPORTED_ENCODING_MESSAGE.to_string(),
        ));
    }
    Ok(decoded.into_owned())
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

        assert_eq!(
            grid[1],
            vec!["ABC-123".to_string(), "resistor, 1%".to_string()]
        );
    }

    #[test]
    fn parses_utf16le_with_bom_prefix() {
        let mut input = vec![0xFF, 0xFE];
        for unit in "mpn,qty\nABC-123,2\n".encode_utf16() {
            input.extend_from_slice(&unit.to_le_bytes());
        }

        let grid = read_csv(&input).expect("utf-16le csv should parse");

        assert_eq!(grid[0], vec!["mpn".to_string(), "qty".to_string()]);
        assert_eq!(grid[1], vec!["ABC-123".to_string(), "2".to_string()]);
    }

    #[test]
    fn decodes_windows_1252_bytes() {
        let input = b"mpn,desc\nABC-123,\x93quoted\x94\n";
        let grid = read_csv(input).expect("windows-1252 csv should parse");

        assert_eq!(grid[1][1], "“quoted”");
    }
}
