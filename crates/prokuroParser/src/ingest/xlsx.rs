use std::io::Cursor;

use calamine::{Data, Reader, Xlsx, XlsxError, open_workbook_from_rs};

use super::ParseError;

#[allow(clippy::type_complexity)]
pub fn read_xlsx(bytes: &[u8]) -> Result<Vec<(String, Vec<Vec<String>>)>, ParseError> {
    let cursor = Cursor::new(bytes.to_vec());
    let mut workbook: Xlsx<_> = open_workbook_from_rs(cursor).map_err(map_xlsx_error)?;

    let mut sheets = Vec::new();
    for sheet_name in workbook.sheet_names() {
        let range = workbook
            .worksheet_range(&sheet_name)
            .map_err(map_xlsx_error)?;
        let grid = range
            .rows()
            .map(|row| row.iter().map(data_to_string).collect())
            .collect();
        sheets.push((sheet_name, grid));
    }

    Ok(sheets)
}

fn map_xlsx_error(error: XlsxError) -> ParseError {
    match error {
        XlsxError::Password => {
            ParseError::PasswordProtected("Workbook is password protected".to_string())
        }
        other => ParseError::Xlsx(other),
    }
}

fn data_to_string(cell: &Data) -> String {
    cell.to_string()
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;

    use super::read_xlsx;

    #[test]
    fn reads_all_sheets_from_minimal_fixture() {
        let fixture_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join("fixtures")
            .join("minimal.xlsx");
        let bytes = fs::read(&fixture_path).expect("fixture should be readable");

        let sheets = read_xlsx(&bytes).expect("xlsx fixture should parse");

        assert_eq!(sheets.len(), 2);
        assert_eq!(sheets[0].0, "BOM");
        assert_eq!(sheets[1].0, "Notes");
        assert_eq!(sheets[0].1[0], vec!["mpn".to_string(), "".to_string(), "qty".to_string()]);
        assert_eq!(
            sheets[0].1[1],
            vec!["RC0603FR-0710KL".to_string(), "".to_string(), "5".to_string()]
        );
        assert_eq!(sheets[1].1[0], vec!["note".to_string()]);
    }
}
