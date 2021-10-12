use conduit::{header, Body, HeaderMap, Method, RequestExt, Response, StatusCode};
use conduit_middleware::{AfterResult, Middleware};
use std::borrow::Cow;
use time::{OffsetDateTime, ParseError, PrimitiveDateTime};

#[allow(missing_copy_implementations)]
pub struct ConditionalGet;

impl Middleware for ConditionalGet {
    fn after(&self, req: &mut dyn RequestExt, res: AfterResult) -> AfterResult {
        let res = res?;

        match *req.method() {
            Method::GET | Method::HEAD => {
                if is_ok(&res) && is_fresh(req, &res) {
                    let (mut parts, _) = res.into_parts();
                    parts.status = StatusCode::NOT_MODIFIED;
                    parts.headers.remove(header::CONTENT_TYPE);
                    parts.headers.remove(header::CONTENT_LENGTH);
                    return Ok(Response::from_parts(parts, Body::empty()));
                }
            }
            _ => (),
        }

        Ok(res)
    }
}

fn is_ok(response: &Response<Body>) -> bool {
    response.status() == 200
}

fn is_fresh(req: &dyn RequestExt, res: &Response<Body>) -> bool {
    let modified_since = get_and_concat_header(req.headers(), header::IF_MODIFIED_SINCE);
    let none_match = get_and_concat_header(req.headers(), header::IF_NONE_MATCH);

    if modified_since.is_empty() && none_match.is_empty() {
        return false;
    }

    let is_modified_since = match std::str::from_utf8(&modified_since) {
        Err(_) => true,
        Ok(string) if string.is_empty() => true,
        Ok(modified_since) => {
            let modified_since = parse_http_date(modified_since);
            match modified_since {
                Err(_) => return false, // Preserve existing behavior
                Ok(parsed) => is_modified_since(parsed, res),
            }
        }
    };

    is_modified_since && etag_matches(&none_match, res)
}

fn etag_matches(none_match: &[u8], res: &Response<Body>) -> bool {
    let value = get_and_concat_header(res.headers(), header::ETAG);
    value == none_match
}

fn is_modified_since(modified_since: OffsetDateTime, res: &Response<Body>) -> bool {
    let last_modified = get_and_concat_header(res.headers(), header::LAST_MODIFIED);

    match std::str::from_utf8(&last_modified) {
        Err(_) => false,
        Ok(last_modified) => match parse_http_date(last_modified) {
            Err(_) => false,
            Ok(last_modified) => modified_since.unix_timestamp() >= last_modified.unix_timestamp(),
        },
    }
}

fn get_and_concat_header(headers: &HeaderMap, name: header::HeaderName) -> Cow<'_, [u8]> {
    let mut values = headers.get_all(name).iter();
    if values.size_hint() == (1, Some(1)) {
        // Exactly 1 value, allocation is unnecessary
        // Unwrap will not panic, because there is a value
        Cow::Borrowed(values.next().unwrap().as_bytes())
    } else {
        let values: Vec<_> = values.map(|val| val.as_bytes()).collect();
        Cow::Owned(values.concat())
    }
}

fn parse_http_date(string: &str) -> Result<OffsetDateTime, ()> {
    parse_rfc1123(string)
        .or_else(|_| parse_rfc850(string))
        .or_else(|_| parse_asctime(string))
        .map_err(|_| ())
}

fn parse_rfc1123(string: &str) -> Result<OffsetDateTime, ParseError> {
    Ok(PrimitiveDateTime::parse(string, "%a, %d %b %Y %T GMT")?.assume_utc())
}

fn parse_rfc850(string: &str) -> Result<OffsetDateTime, ParseError> {
    Ok(PrimitiveDateTime::parse(string, "%a, %d-%m-%y %T GMT")?.assume_utc())
}

fn parse_asctime(string: &str) -> Result<OffsetDateTime, ParseError> {
    // TODO: should this be "%a %b %d %T %Y"?
    Ok(PrimitiveDateTime::parse(string, "%a %m\t%d %T %Y")?.assume_utc())
}

#[cfg(test)]
mod tests {
    use conduit::{
        box_error, header, Body, Handler, HandlerResult, HeaderMap, Method, RequestExt, Response,
        StatusCode,
    };
    use conduit_middleware::MiddlewareBuilder;
    use conduit_test::{MockRequest, ResponseExt};
    use time::{Duration, OffsetDateTime};

    use super::ConditionalGet;

    macro_rules! returning {
        ($status:expr, $($header:expr => $value:expr),+) => ({
            use std::convert::TryInto;
            let mut headers = HeaderMap::new();
            $(headers.append($header, $value.try_into().unwrap());)+
            let handler = SimpleHandler::new(headers, $status, "hello");
            let mut stack = MiddlewareBuilder::new(handler);
            stack.add(ConditionalGet);
            stack
        });
        ($($header:expr => $value:expr),+) => ({
            returning!(StatusCode::OK, $($header => $value),+)
        })
    }

    macro_rules! request {
        ($($header:expr => $value:expr),+) => ({
            let mut req = MockRequest::new(Method::GET, "/");
            $(req.header($header, &$value.to_string());)+
            req
        })
    }

    #[test]
    fn test_sends_304() {
        let handler = returning!(header::LAST_MODIFIED => httpdate(OffsetDateTime::now_utc()));
        expect_304(handler.call(&mut request!(
            header::IF_MODIFIED_SINCE => httpdate(OffsetDateTime::now_utc())
        )));
    }

    #[test]
    fn test_sends_304_if_older_than_now() {
        let handler = returning!(header::LAST_MODIFIED => before_now());
        expect_304(handler.call(&mut request!(
            header::IF_MODIFIED_SINCE => httpdate(OffsetDateTime::now_utc())
        )));
    }

    #[test]
    fn test_sends_304_with_etag() {
        let handler = returning!(header::ETAG => "1234");
        expect_304(handler.call(&mut request!(
            header::IF_NONE_MATCH => "1234"
        )));
    }

    #[test]
    fn test_sends_200_with_fresh_time_but_not_etag() {
        let handler = returning!(header::LAST_MODIFIED => before_now(), header::ETAG => "1234");
        expect_200(handler.call(&mut request!(
            header::IF_MODIFIED_SINCE => now(),
            header::IF_NONE_MATCH => "4321"
        )));
    }

    #[test]
    fn test_sends_200_with_fresh_etag_but_not_time() {
        let handler = returning!(header::LAST_MODIFIED => now(), header::ETAG => "1234");
        expect_200(handler.call(&mut request!(
            header::IF_MODIFIED_SINCE => before_now(),
            header::IF_NONE_MATCH => "1234"
        )));
    }

    #[test]
    fn test_sends_200_with_fresh_etag() {
        let handler = returning!(header::ETAG => "1234");
        expect_200(handler.call(&mut request!(
            header::IF_NONE_MATCH => "4321"
        )));
    }

    #[test]
    fn test_sends_200_with_fresh_time() {
        let handler = returning!(header::LAST_MODIFIED => now());
        expect_200(handler.call(&mut request!(
            header::IF_MODIFIED_SINCE => before_now()
        )));
    }

    #[test]
    fn test_sends_304_with_fresh_time_and_etag() {
        let handler = returning!(header::LAST_MODIFIED => before_now(), header::ETAG => "1234");
        expect_304(handler.call(&mut request!(
            header::IF_MODIFIED_SINCE => now(),
            header::IF_NONE_MATCH => "1234"
        )));
    }

    #[test]
    fn test_does_not_affect_non_200() {
        let handler = returning!(StatusCode::FOUND, header::LAST_MODIFIED => before_now(), header::ETAG => "1234");
        expect(
            StatusCode::FOUND,
            handler.call(&mut request!(
                header::IF_MODIFIED_SINCE => now(),
                header::IF_NONE_MATCH => "1234"
            )),
        );
    }

    #[test]
    fn test_does_not_affect_malformed_timestamp() {
        let bad_stamp = OffsetDateTime::now_utc().format("%Y-%m-%d %H:%M:%S %z");
        let handler = returning!(header::LAST_MODIFIED => before_now());
        expect_200(handler.call(&mut request!(
            header::IF_MODIFIED_SINCE => bad_stamp
        )));
    }

    fn expect_304(response: HandlerResult) {
        let response = response.expect("No response");
        assert_eq!(response.status(), StatusCode::NOT_MODIFIED);
        assert_eq!(*response.into_cow(), b""[..]);
    }

    fn expect_200(response: HandlerResult) {
        expect(StatusCode::OK, response);
    }

    fn expect(status: StatusCode, response: HandlerResult) {
        let response = response.expect("No response");
        assert_eq!(response.status(), status);
        assert_eq!(*response.into_cow(), b"hello"[..]);
    }

    struct SimpleHandler {
        headers: HeaderMap,
        status: StatusCode,
        body: &'static str,
    }

    impl SimpleHandler {
        fn new(headers: HeaderMap, status: StatusCode, body: &'static str) -> SimpleHandler {
            SimpleHandler {
                headers,
                status,
                body,
            }
        }
    }

    impl Handler for SimpleHandler {
        fn call(&self, _: &mut dyn RequestExt) -> HandlerResult {
            let mut builder = Response::builder().status(self.status);
            builder.headers_mut().unwrap().extend(self.headers.clone());
            builder
                .body(Body::from_static(self.body.as_bytes()))
                .map_err(box_error)
        }
    }

    fn before_now() -> String {
        let now = OffsetDateTime::now_utc();
        httpdate(now - Duration::weeks(52))
    }

    fn now() -> String {
        httpdate(OffsetDateTime::now_utc())
    }

    fn httpdate(time: OffsetDateTime) -> String {
        time.format("%a, %d-%m-%y %T GMT")
    }
}
