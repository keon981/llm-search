//! Country hint mapping between ISO 3166-1 alpha-2 codes (Brave `country`)
//! and Tavily's `country` enum of lower-case English names.
//!
//! The name list is Tavily's documented enum (166 values, search endpoint,
//! fetched 2026-11-12); a country outside it has no Tavily spelling, so the
//! hint is dropped for Tavily with a warning.

const COUNTRIES: &[(&str, &str)] = &[
    ("AF", "afghanistan"),
    ("AL", "albania"),
    ("DZ", "algeria"),
    ("AD", "andorra"),
    ("AO", "angola"),
    ("AR", "argentina"),
    ("AM", "armenia"),
    ("AU", "australia"),
    ("AT", "austria"),
    ("AZ", "azerbaijan"),
    ("BS", "bahamas"),
    ("BH", "bahrain"),
    ("BD", "bangladesh"),
    ("BB", "barbados"),
    ("BY", "belarus"),
    ("BE", "belgium"),
    ("BZ", "belize"),
    ("BJ", "benin"),
    ("BT", "bhutan"),
    ("BO", "bolivia"),
    ("BA", "bosnia and herzegovina"),
    ("BW", "botswana"),
    ("BR", "brazil"),
    ("BN", "brunei"),
    ("BG", "bulgaria"),
    ("BF", "burkina faso"),
    ("BI", "burundi"),
    ("KH", "cambodia"),
    ("CM", "cameroon"),
    ("CA", "canada"),
    ("CV", "cape verde"),
    ("CF", "central african republic"),
    ("TD", "chad"),
    ("CL", "chile"),
    ("CN", "china"),
    ("CO", "colombia"),
    ("KM", "comoros"),
    ("CG", "congo"),
    ("CR", "costa rica"),
    ("HR", "croatia"),
    ("CU", "cuba"),
    ("CY", "cyprus"),
    ("CZ", "czech republic"),
    ("DK", "denmark"),
    ("DJ", "djibouti"),
    ("DO", "dominican republic"),
    ("EC", "ecuador"),
    ("EG", "egypt"),
    ("SV", "el salvador"),
    ("GQ", "equatorial guinea"),
    ("ER", "eritrea"),
    ("EE", "estonia"),
    ("ET", "ethiopia"),
    ("FJ", "fiji"),
    ("FI", "finland"),
    ("FR", "france"),
    ("GA", "gabon"),
    ("GM", "gambia"),
    ("GE", "georgia"),
    ("DE", "germany"),
    ("GH", "ghana"),
    ("GR", "greece"),
    ("GT", "guatemala"),
    ("GN", "guinea"),
    ("HT", "haiti"),
    ("HN", "honduras"),
    ("HU", "hungary"),
    ("IS", "iceland"),
    ("IN", "india"),
    ("ID", "indonesia"),
    ("IR", "iran"),
    ("IQ", "iraq"),
    ("IE", "ireland"),
    ("IL", "israel"),
    ("IT", "italy"),
    ("JM", "jamaica"),
    ("JP", "japan"),
    ("JO", "jordan"),
    ("KZ", "kazakhstan"),
    ("KE", "kenya"),
    ("KW", "kuwait"),
    ("KG", "kyrgyzstan"),
    ("LV", "latvia"),
    ("LB", "lebanon"),
    ("LS", "lesotho"),
    ("LR", "liberia"),
    ("LY", "libya"),
    ("LI", "liechtenstein"),
    ("LT", "lithuania"),
    ("LU", "luxembourg"),
    ("MG", "madagascar"),
    ("MW", "malawi"),
    ("MY", "malaysia"),
    ("MV", "maldives"),
    ("ML", "mali"),
    ("MT", "malta"),
    ("MR", "mauritania"),
    ("MU", "mauritius"),
    ("MX", "mexico"),
    ("MD", "moldova"),
    ("MC", "monaco"),
    ("MN", "mongolia"),
    ("ME", "montenegro"),
    ("MA", "morocco"),
    ("MZ", "mozambique"),
    ("MM", "myanmar"),
    ("NA", "namibia"),
    ("NP", "nepal"),
    ("NL", "netherlands"),
    ("NZ", "new zealand"),
    ("NI", "nicaragua"),
    ("NE", "niger"),
    ("NG", "nigeria"),
    ("KP", "north korea"),
    ("MK", "north macedonia"),
    ("NO", "norway"),
    ("OM", "oman"),
    ("PK", "pakistan"),
    ("PA", "panama"),
    ("PG", "papua new guinea"),
    ("PY", "paraguay"),
    ("PE", "peru"),
    ("PH", "philippines"),
    ("PL", "poland"),
    ("PT", "portugal"),
    ("QA", "qatar"),
    ("RO", "romania"),
    ("RU", "russia"),
    ("RW", "rwanda"),
    ("SA", "saudi arabia"),
    ("SN", "senegal"),
    ("RS", "serbia"),
    ("SG", "singapore"),
    ("SK", "slovakia"),
    ("SI", "slovenia"),
    ("SO", "somalia"),
    ("ZA", "south africa"),
    ("KR", "south korea"),
    ("SS", "south sudan"),
    ("ES", "spain"),
    ("LK", "sri lanka"),
    ("SD", "sudan"),
    ("SE", "sweden"),
    ("CH", "switzerland"),
    ("SY", "syria"),
    ("TW", "taiwan"),
    ("TJ", "tajikistan"),
    ("TZ", "tanzania"),
    ("TH", "thailand"),
    ("TG", "togo"),
    ("TT", "trinidad and tobago"),
    ("TN", "tunisia"),
    ("TR", "turkey"),
    ("TM", "turkmenistan"),
    ("UG", "uganda"),
    ("UA", "ukraine"),
    ("AE", "united arab emirates"),
    ("GB", "united kingdom"),
    ("US", "united states"),
    ("UY", "uruguay"),
    ("UZ", "uzbekistan"),
    ("VE", "venezuela"),
    ("VN", "vietnam"),
    ("YE", "yemen"),
    ("ZM", "zambia"),
    ("ZW", "zimbabwe"),
];

/// Tavily's spelling for a country given as an ISO code or an English name.
pub fn tavily_country(input: &str) -> Option<&'static str> {
    let value = input.trim();
    COUNTRIES
        .iter()
        .find(|(code, name)| code.eq_ignore_ascii_case(value) || name.eq_ignore_ascii_case(value))
        .map(|(_, name)| *name)
}

/// ISO alpha-2 code (upper case) for Brave. A two-letter input is passed
/// through as given; a name is looked up in the table.
pub fn brave_country(input: &str) -> Option<String> {
    let value = input.trim();
    if value.len() == 2 && value.chars().all(|c| c.is_ascii_alphabetic()) {
        return Some(value.to_ascii_uppercase());
    }
    COUNTRIES
        .iter()
        .find(|(_, name)| name.eq_ignore_ascii_case(value))
        .map(|(code, _)| (*code).to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn table_matches_tavily_enum_size_and_is_unique() {
        assert_eq!(COUNTRIES.len(), 166);
        let codes: HashSet<_> = COUNTRIES.iter().map(|(code, _)| code).collect();
        let names: HashSet<_> = COUNTRIES.iter().map(|(_, name)| name).collect();
        assert_eq!(codes.len(), 166);
        assert_eq!(names.len(), 166);
    }

    #[test]
    fn maps_both_directions() {
        assert_eq!(tavily_country("tw"), Some("taiwan"));
        assert_eq!(tavily_country("United Kingdom"), Some("united kingdom"));
        assert_eq!(tavily_country("XX"), None);
        assert_eq!(brave_country("de").as_deref(), Some("DE"));
        assert_eq!(brave_country("south korea").as_deref(), Some("KR"));
        assert_eq!(brave_country("atlantis"), None);
    }
}
