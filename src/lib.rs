#![feature(try_trait)]

use lazy_static::lazy_static;
use scraper::Selector;

lazy_static! {
    static ref ERROR_MESSAGE: Selector = Selector::parse(".error-message-box, div#standardpage section.notice-message p.link-override").unwrap();
    // use inner text
    static ref ARTIST: Selector = Selector::parse(".submission-id-sub-container .submission-title + a").unwrap();
    // use src attribute
    static ref IMAGE_URL: Selector = Selector::parse("#submissionImg").unwrap();
    static ref FLASH_OBJECT: Selector = Selector::parse("#flash_embed").unwrap();
    // use title attribute
    static ref POSTED_AT: Selector = Selector::parse(".submission-id-sub-container strong span.popup_date").unwrap();
    // get all, use inner text
    static ref TAGS: Selector = Selector::parse("section.tags-row a").unwrap();
    // html description, includes unneeded .submission-title div but unsure how best to remove
    static ref DESCRIPTION: Selector = Selector::parse(".section-body.bg2").unwrap();
    // submission rating, use inner text
    static ref RATING: Selector = Selector::parse(".stats-container .rating span.rating-box").unwrap();

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
        Self::new(error.to_string(), true)
    }
}

impl From<image::ImageError> for Error {
    fn from(error: image::ImageError) -> Self {
        Self::new(error.to_string(), false)
    }
}

impl From<std::option::NoneError> for Error {
    fn from(_error: std::option::NoneError) -> Self {
        Self::new("unable to find value", true)
    }
}

impl From<std::num::ParseIntError> for Error {
    fn from(_error: std::num::ParseIntError) -> Self {
        Self::new("value was not number", false)
    }
}

pub struct FurAffinity {
    cookie_a: String,
    cookie_b: String,
    user_agent: String,

    client: reqwest::Client,
}

impl FurAffinity {
    pub fn new<T>(cookie_a: T, cookie_b: T, user_agent: T) -> Self
    where
        T: Into<String>,
    {
        Self {
            cookie_a: cookie_a.into(),
            cookie_b: cookie_b.into(),
            user_agent: user_agent.into(),
            client: reqwest::Client::new(),
        }
    }

    fn get_cookies(&self) -> String {
        [
            build_cookie("a", &self.cookie_a),
            build_cookie("b", &self.cookie_b),
        ]
        .join("; ")
    }

    pub async fn load_page(&self, url: &str) -> reqwest::Result<reqwest::Response> {
        use reqwest::header;

        self.client
            .get(url)
            .header(header::USER_AGENT, &self.user_agent)
            .header(header::COOKIE, self.get_cookies())
            .send()
            .await
    }

    pub async fn latest_id(&self) -> Result<i32, Error> {
        let page = self.load_page("https://www.furaffinity.net/").await?;

        if page.status().is_server_error() {
            return Err(Error::new(
                format!("got server error: {}", page.status()),
                true,
            ));
        }

        let document = scraper::Html::parse_document(&page.text().await?);
        let latest = document.select(&LATEST_SUBMISSION).next()?;

        let id = latest
            .value()
            .attr("href")?
            .split('/')
            .filter(|part| !part.is_empty())
            .last()?;

        Ok(id.parse()?)
    }

    pub async fn get_submission(&self, id: i32) -> Result<Option<Submission>, Error> {
        let page = self
            .load_page(&format!("https://www.furaffinity.net/view/{}", id))
            .await?;

        if page.status().is_server_error() {
            return Err(Error::new(
                format!("got server error: {}", page.status()),
                true,
            ));
        }

        parse_submission(id, &page.text().await?)
    }

    pub async fn calc_image_hash(&self, sub: Submission) -> Result<Submission, Error> {
        let url = match &sub.content {
            Content::Flash(_) => return Ok(Submission { hash: None, ..sub }),
            Content::Image(url) => url.clone(),
        };

        let image = self.load_page(&url).await?;

        if image.status().is_server_error() {
            return Err(Error::new(
                format!("got server error: {}", image.status()),
                true,
            ));
        }

        let buf = image.bytes().await?.to_vec();

        hash_image(&buf).map(|hash| {
            let mut bytes: [u8; 8] = [0; 8];
            bytes.copy_from_slice(hash.as_bytes());

            let num = i64::from_be_bytes(bytes);

            Submission {
                hash: Some(hash.to_base64()),
                hash_num: Some(num),
                ..sub
            }
        })
    }
}

fn extract_url(elem: scraper::ElementRef, attr: &'static str) -> (String, String, String) {
    let url = "https:".to_owned()
        + elem
            .value()
            .attr(attr)
            .expect("unable to get src attribute");

    let url_ext = url.split('.').last().unwrap_or("a").to_string();
    let filename = url.split('/').last().unwrap().to_string();

    (url, url_ext, filename)
}

pub fn parse_submission(id: i32, page: &str) -> Result<Option<Submission>, Error> {
    let document = scraper::Html::parse_document(page);

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

    let tags = document.select(&TAGS).collect::<Vec<_>>();
    let tags: Vec<String> = tags.into_iter().map(join_text_nodes).collect();

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
        hash_num: None,
        filename,
        rating,
        posted_at: parse_date(&posted_at),
        tags,
        description,
    }))
}

pub fn get_hasher() -> img_hash::Hasher<[u8; 8]> {
    img_hash::HasherConfig::with_bytes_type::<[u8; 8]>()
        .hash_alg(img_hash::HashAlg::Gradient)
        .hash_size(8, 8)
        .preproc_dct()
        .to_hasher()
}

pub fn hash_image(image: &[u8]) -> Result<img_hash::ImageHash<[u8; 8]>, Error> {
    let hasher = get_hasher();

    let image = image::load_from_memory(image)?;
    let hash = hasher.hash_image(&image);

    Ok(hash)
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

#[derive(Clone, Debug, PartialEq)]
pub enum Content {
    Image(String),
    Flash(String),
}

impl Content {
    /// Extract URL from any type of Content.
    pub fn url(&self) -> String {
        match self {
            Content::Image(url) => url.clone(),
            Content::Flash(url) => url.clone(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct Submission {
    pub id: i32,
    pub artist: String,
    pub content: Content,
    pub ext: String,
    pub hash: Option<String>,
    pub hash_num: Option<i64>,
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
    fn test_load_submission() {
        let mut runtime = tokio::runtime::Runtime::new().unwrap();

        let fa = FurAffinity::new("", "", "furaffinity-rs test");
        let sub = runtime
            .block_on(fa.get_submission(31209021))
            .expect("unable to load test submission")
            .expect("submission did not exist");

        assert_eq!(sub.artist, "deadrussiansoul");
        assert_eq!(sub.content, Content::Image("https://d.facdn.net/art/deadrussiansoul/1555431774/1555431774.deadrussiansoul_Скан_20190411__7_.png".into()));
        assert_eq!(sub.tags, vec!["fox", "bilberry"]);

        let sub = runtime
            .block_on(fa.get_submission(34426892))
            .expect("unable to load submission");

        assert!(sub.is_none());
    }

    #[test]
    fn test_parse_date() {
        use chrono::offset::TimeZone;

        let parsed = parse_date("Mar 23rd, 2019 12:46 AM");
        assert_eq!(parsed, chrono::Utc.ymd(2019, 3, 23).and_hms(5, 46, 0));
    }
}
