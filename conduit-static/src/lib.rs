use conduit::{box_error, header, Body, Handler, HandlerResult, RequestExt, Response, StatusCode};
use conduit_mime_types as mime;
use filetime::FileTime;
use std::fs::File;
use std::path::{Path, PathBuf};
use time::OffsetDateTime;

pub struct Static {
    path: PathBuf,
    types: mime::Types,
}

impl Static {
    pub fn new<P: AsRef<Path>>(path: P) -> Static {
        Static {
            path: path.as_ref().to_path_buf(),
            types: mime::Types::new().expect("Couldn't load mime-types"),
        }
    }
}

impl Handler for Static {
    fn call(&self, request: &mut dyn RequestExt) -> HandlerResult {
        let request_path = &request.path()[1..];
        if request_path.contains("..") {
            return Ok(not_found());
        }

        let path = self.path.join(request_path);
        let mime = self.types.mime_for_path(&path);
        let file = match File::open(&path) {
            Ok(f) => f,
            Err(..) => return Ok(not_found()),
        };
        let data = file.metadata().map_err(box_error)?;
        if data.is_dir() {
            return Ok(not_found());
        }
        let mtime = FileTime::from_last_modification_time(&data);
        let mtime = OffsetDateTime::from_unix_timestamp(mtime.unix_seconds() as i64);

        Response::builder()
            .header(header::CONTENT_TYPE, mime)
            .header(header::CONTENT_LENGTH, data.len())
            .header(header::LAST_MODIFIED, mtime.format("%a, %d %b %Y %T GMT"))
            .body(Body::File(file))
            .map_err(box_error)
    }
}

fn not_found() -> Response<Body> {
    Response::builder()
        .status(StatusCode::NOT_FOUND)
        .header(header::CONTENT_LENGTH, 0)
        .header(header::CONTENT_TYPE, "text/plain")
        .body(Body::empty())
        .unwrap()
}

#[cfg(test)]
mod tests {
    use std::fs::{self, File};
    use std::io::prelude::*;
    use tempdir::TempDir;

    use crate::Static;
    use conduit::{header, Handler, Method, StatusCode};
    use conduit_test::{MockRequest, ResponseExt};

    #[test]
    fn test_static() {
        let td = TempDir::new("conduit-static").unwrap();
        let root = td.path();
        let handler = Static::new(root);
        File::create(&root.join("Cargo.toml"))
            .unwrap()
            .write_all(b"[package]")
            .unwrap();
        let mut req = MockRequest::new(Method::GET, "/Cargo.toml");
        let res = handler.call(&mut req).expect("No response");
        assert_eq!(
            res.headers().get(header::CONTENT_TYPE).unwrap(),
            "text/plain"
        );
        assert_eq!(res.headers().get(header::CONTENT_LENGTH).unwrap(), "9");
        assert_eq!(*res.into_cow(), b"[package]"[..]);
    }

    #[test]
    fn test_mime_types() {
        let td = TempDir::new("conduit-static").unwrap();
        let root = td.path();
        fs::create_dir(&root.join("src")).unwrap();
        File::create(&root.join("src/fixture.css")).unwrap();

        let handler = Static::new(root);
        let mut req = MockRequest::new(Method::GET, "/src/fixture.css");
        let res = handler.call(&mut req).expect("No response");
        assert_eq!(res.headers().get(header::CONTENT_TYPE).unwrap(), "text/css");
        assert_eq!(res.headers().get(header::CONTENT_LENGTH).unwrap(), "0");
    }

    #[test]
    fn test_missing() {
        let td = TempDir::new("conduit-static").unwrap();
        let root = td.path();

        let handler = Static::new(root);
        let mut req = MockRequest::new(Method::GET, "/nope");
        let res = handler.call(&mut req).expect("No response");
        assert_eq!(res.status(), StatusCode::NOT_FOUND);
    }

    #[test]
    fn test_dir() {
        let td = TempDir::new("conduit-static").unwrap();
        let root = td.path();

        fs::create_dir(&root.join("foo")).unwrap();

        let handler = Static::new(root);
        let mut req = MockRequest::new(Method::GET, "/foo");
        let res = handler.call(&mut req).expect("No response");
        assert_eq!(res.status(), StatusCode::NOT_FOUND);
    }

    #[test]
    fn last_modified() {
        let td = TempDir::new("conduit-static").unwrap();
        let root = td.path();
        File::create(&root.join("test")).unwrap();
        let handler = Static::new(root);
        let mut req = MockRequest::new(Method::GET, "/test");
        let res = handler.call(&mut req).expect("No response");
        assert_eq!(res.status(), StatusCode::OK);
        assert!(res.headers().get(header::LAST_MODIFIED).is_some());
    }
}
