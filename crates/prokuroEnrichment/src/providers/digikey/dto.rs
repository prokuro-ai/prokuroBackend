//! Digi-Key ProductDetails JSON shapes (subset we map).

use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct ProductDetailsResponse {
    #[serde(rename = "Product")]
    pub product: Option<Product>,
}

#[derive(Debug, Deserialize)]
pub struct Product {
    #[serde(rename = "DigiKeyProductNumber")]
    pub digi_key_product_number: Option<String>,
    #[serde(rename = "ManufacturerProductNumber")]
    pub manufacturer_product_number: Option<String>,
    #[serde(rename = "Manufacturer")]
    pub manufacturer: Option<Named>,
    #[serde(rename = "QuantityAvailable")]
    pub quantity_available: Option<i64>,
    #[serde(rename = "ManufacturerLeadWeeks")]
    pub manufacturer_lead_weeks: Option<String>,
    #[serde(rename = "ProductStatus")]
    pub product_status: Option<ProductStatus>,
    #[serde(rename = "Discontinued")]
    pub discontinued: Option<bool>,
    #[serde(rename = "EndOfLife")]
    pub end_of_life: Option<bool>,
    #[serde(rename = "Classifications")]
    pub classifications: Option<Classifications>,
    #[serde(rename = "Category")]
    pub category: Option<Named>,
    #[serde(rename = "CountryOfOrigin")]
    pub country_of_origin: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct Named {
    #[serde(rename = "Name")]
    pub name: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ProductStatus {
    #[serde(rename = "Status")]
    pub status: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct Classifications {
    #[serde(rename = "HtsusCode")]
    pub htsus_code: Option<String>,
}
