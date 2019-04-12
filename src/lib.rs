use lazy_static::lazy_static;
use scraper::Selector;

lazy_static! {
    static ref ERROR_MESSAGE: Selector = Selector::parse(".error-message-box").unwrap();
    // use inner text
    static ref ARTIST: Selector = Selector::parse(".submission-artist-container a h2").unwrap();
    // use src attribute
    static ref IMAGE_URL: Selector = Selector::parse("#submissionImg").unwrap();
    static ref FLASH_OBJECT: Selector = Selector::parse("#flash_embed").unwrap();
    // use title attribute
    static ref POSTED_AT: Selector = Selector::parse(".submission-title strong span.popup_date").unwrap();
    // get all, use inner text
    static ref TAGS: Selector = Selector::parse(".submission-sidebar .tags a").unwrap();
    // html description, includes unneeded .submission-title div but unsure how best to remove
    static ref DESCRIPTION: Selector = Selector::parse(".submission-description-container").unwrap();
    // submission rating, use inner text
    static ref RATING: Selector = Selector::parse(".submission-description .rating-box").unwrap();

    static ref LATEST_SUBMISSION: Selector = Selector::parse("#gallery-frontpage-submissions figure:first-child b u a").unwrap();

    static ref DATE_CLEANER: regex::Regex = regex::Regex::new(r"(\d{1,2})(st|nd|rd|th)").unwrap();
}

#[derive(Debug)]
pub struct Error {
    pub message: String,
    pub retry: bool,
}

impl Error {
    fn new<T>(message: T, retry: bool) -> Self
    where
        T: Into<String>,
    {
        Self {
            message: message.into(),
            retry,
        }
    }
}

impl From<reqwest::Error> for Error {
    fn from(error: reqwest::Error) -> Self {
        use std::error::Error;

        Self::new(error.description(), true)
    }
}

impl From<image::ImageError> for Error {
    fn from(error: image::ImageError) -> Self {
        use std::error::Error;

        Self::new(error.description(), false)
    }
}

pub struct FurAffinity {
    cookie_a: String,
    cookie_b: String,

    client: reqwest::Client,
}

impl FurAffinity {
    pub fn new<T>(cookie_a: T, cookie_b: T) -> Self
    where
        T: Into<String>,
    {
        Self {
            cookie_a: cookie_a.into(),
            cookie_b: cookie_b.into(),
            client: reqwest::Client::new(),
        }
    }

    fn get_cookies(&self) -> String {
        [
            build_cookie("a".into(), &self.cookie_a),
            build_cookie("b".into(), &self.cookie_b),
        ]
        .join("; ")
    }

    pub fn load_page(&self, url: &str) -> reqwest::Result<reqwest::Response> {
        use reqwest::header;

        self.client
            .get(url)
            .header(header::COOKIE, self.get_cookies())
            .send()
    }

    pub fn latest_id(&self) -> Result<i32, Error> {
        let mut page = self.load_page("https://www.furaffinity.net/")?;
        if page.status().is_server_error() {
            return Err(Error::new(
                format!("got server error: {}", page.status()),
                true,
            ));
        }

        let document = scraper::Html::parse_document(&page.text()?);
        let latest = document
            .select(&LATEST_SUBMISSION)
            .next()
            .expect("unable to get latest submission");

        let id = latest
            .value()
            .attr("href")
            .expect("unable to get href")
            .split("/")
            .into_iter()
            .filter(|part| part.len() > 0)
            .last()
            .expect("no id found");

        Ok(id.parse().expect("unable to get id from href"))
    }

    pub fn get_submission(&self, id: i32) -> Result<Option<Submission>, Error> {
        let mut page = self.load_page(&format!("https://www.furaffinity.net/view/{}", id))?;
        if page.status().is_server_error() {
            return Err(Error::new(
                format!("got server error: {}", page.status()),
                true,
            ));
        }

        parse_submission(id, &page.text()?)
    }

    pub fn calc_image_hash(&self, sub: Submission) -> Result<Submission, Error> {
        let url = match &sub.content {
            Content::Flash(_) => return Ok(Submission { hash: None, ..sub }),
            Content::Image(url) => url.clone(),
        };

        let mut image = self.load_page(&url)?;
        if image.status().is_server_error() {
            return Err(Error::new(
                format!("got server error: {}", image.status()),
                true,
            ));
        }

        let mut buf = vec![];
        image.copy_to(&mut buf)?;

        hash_image(&buf).map(|hash| Submission {
            hash: Some(hash),
            ..sub
        })
    }
}

fn extract_url(elem: scraper::ElementRef, attr: &'static str) -> (String, String, String) {
    let url = "https:".to_owned()
        + elem
            .value()
            .attr(attr)
            .expect("unable to get src attribute");
    let url_ext = url.split(".").last().unwrap_or("a").to_string();
    let filename = url.split("/").last().unwrap().to_string();

    (url, url_ext, filename)
}

pub fn parse_submission(id: i32, page: &str) -> Result<Option<Submission>, Error> {
    let document = scraper::Html::parse_document(page);

    // println!("{}", document.root_element().html());

    if document.select(&ERROR_MESSAGE).next().is_some() {
        return Ok(None);
    }

    let artist = match document.select(&ARTIST).next() {
        Some(artist) => join_text_nodes(artist),
        None => return Err(Error::new("unable to select artist", false)),
    };

    let (content, url_ext, filename) = {
        if let Some(url) = document.select(&IMAGE_URL).next() {
            let (url, url_ext, filename) = extract_url(url, "src");

            (Content::Image(url), url_ext, filename)
        } else if let Some(url) = document.select(&FLASH_OBJECT).next() {
            let (url, url_ext, filename) = extract_url(url, "data");

            (Content::Flash(url), url_ext, filename)
        } else {
            panic!("invalid submission type")
        }
    };

    let rating = match document.select(&RATING).next() {
        Some(rating) => Rating::parse(&join_text_nodes(rating)).unwrap(),
        None => return Err(Error::new("unable to select submission rating", false)),
    };

    let posted_at = match document.select(&POSTED_AT).next() {
        Some(posted_at) => posted_at.value().attr("title").unwrap().to_string(),
        None => return Err(Error::new("unable to select posted at", false)),
    };

    let tags = document.select(&TAGS).into_iter().collect::<Vec<_>>();
    let tags: Vec<String> = tags.into_iter().map(|elem| join_text_nodes(elem)).collect();

    let description = match document.select(&DESCRIPTION).next() {
        Some(description) => description.inner_html(),
        None => return Err(Error::new("unable to select description", false)),
    };

    Ok(Some(Submission {
        id,
        artist,
        content,
        ext: url_ext,
        hash: None,
        filename,
        rating,
        posted_at: parse_date(&posted_at),
        tags,
        description,
    }))
}

pub fn hash_image(image: &[u8]) -> Result<String, Error> {
    let hasher = img_hash::HasherConfig::new().to_hasher();

    let image = image::load_from_memory(image)?;
    let hash = hasher.hash_image(&image);

    Ok(hash.to_base64())
}

#[derive(Clone, Debug)]
pub enum Rating {
    General,
    Mature,
    Adult,
}

impl Rating {
    fn parse(name: &str) -> Option<Self> {
        match name {
            "General" => Some(Rating::General),
            "Mature" => Some(Rating::Mature),
            "Adult" => Some(Rating::Adult),
            _ => None,
        }
    }

    pub fn serialize(&self) -> String {
        match self {
            Rating::General => "g".into(),
            Rating::Mature => "m".into(),
            Rating::Adult => "a".into(),
        }
    }
}

#[derive(Clone, Debug)]
pub enum Content {
    Image(String),
    Flash(String),
}

#[derive(Clone, Debug)]
pub struct Submission {
    pub id: i32,
    pub artist: String,
    pub content: Content,
    pub ext: String,
    pub hash: Option<String>,
    pub filename: String,
    pub rating: Rating,
    pub posted_at: chrono::DateTime<chrono::Utc>,
    pub tags: Vec<String>,
    pub description: String,
}

fn build_cookie(name: &str, value: &str) -> String {
    format!("{}={}", name, value)
}

fn join_text_nodes(elem: scraper::ElementRef) -> String {
    elem.text().collect::<Vec<_>>().join("").trim().to_string()
}

pub fn parse_date(date: &str) -> chrono::DateTime<chrono::Utc> {
    use chrono::offset::TimeZone;

    let date_str = DATE_CLEANER.replace(date, "$1");

    let zone = chrono::FixedOffset::west(5 * 3600);
    let date = zone
        .datetime_from_str(&date_str, "%b %e, %Y %l:%M %p")
        .expect("unable to parse date");

    date.with_timezone(&chrono::Utc)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_date() {
        use chrono::offset::TimeZone;

        let parsed = parse_date("Mar 23rd, 2019 12:46 AM");
        assert_eq!(parsed, chrono::Utc.ymd(2019, 3, 23).and_hms(5, 46, 0));
    }
}
