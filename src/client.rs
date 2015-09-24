use std::fmt;
use std::ffi::CString;
use std::path::PathBuf;
use std::mem;
use std::ptr;
use std::io;
use std::fs::File;

use mongoc::bindings;

use bson::Document;

use super::Result;
use super::BsoncError;
use super::bsonc::Bsonc;
use super::collection::{Collection,CreatedBy};
use super::database::Database;
use super::uri::Uri;
use super::read_prefs::ReadPrefs;

// TODO: We're using a sort of poor man's Arc here
// with this root bool, there must be a better way.
pub struct ClientPool {
    root_instance: bool,
    // Uri and SslOptions need to be present for the
    // lifetime of this pool.
    uri:          Uri,
    ssl_options:  Option<SslOptions>,
    inner:         *mut bindings::mongoc_client_pool_t
}

impl ClientPool {
    /// Create a new ClientPool with optionally SSL options
    ///
    /// See: http://api.mongodb.org/c/current/mongoc_client_pool_t.html
    /// And: http://api.mongodb.org/c/current/mongoc_ssl_opt_t.html
    pub fn new(uri: Uri, ssl_options: Option<SslOptions>) -> ClientPool {
        super::init();
        let pool = unsafe {
            let pool_ptr = bindings::mongoc_client_pool_new(uri.inner());
            assert!(!pool_ptr.is_null());
            pool_ptr
        };
        match ssl_options {
            Some(ref options) => {
                unsafe {
                    bindings::mongoc_client_pool_set_ssl_opts(
                        pool,
                        options.inner()
                    );
                }
            },
            None => ()
        };
        ClientPool {
            root_instance: true,
            uri:         uri,
            ssl_options: ssl_options,
            inner:       pool
        }
    }

    /// Get a reference to this pool's Uri
    pub fn get_uri(&self) -> &Uri {
        &self.uri
    }

    /// Retrieve a client from the client pool, possibly blocking until one is available.
    /// See: http://api.mongodb.org/c/current/mongoc_client_pool_pop.html
    pub fn pop(&self) -> Client {
        assert!(!self.inner.is_null());
        let client = unsafe { bindings::mongoc_client_pool_pop(self.inner) };
        Client{
            client_pool: self,
            inner:       client
        }
    }

    /// Return a client back to the client pool, called from drop of client.
    /// See: http://api.mongodb.org/c/current/mongoc_client_pool_push.html
    unsafe fn push(&self, mongo_client: *mut bindings::mongoc_client_t) {
        assert!(!self.inner.is_null());
        bindings::mongoc_client_pool_push(
            self.inner,
            mongo_client
        );
    }
}

unsafe impl Send for ClientPool { }
unsafe impl Sync for ClientPool { }

impl fmt::Debug for ClientPool {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "ClientPool for {}", self.uri.as_str())
    }
}

impl Clone for ClientPool {
    fn clone(&self) -> ClientPool {
        assert!(!self.inner.is_null());
        ClientPool {
            root_instance: false,
            uri:           self.uri.clone(),
            ssl_options:   self.ssl_options.clone(),
            inner:         self.inner.clone()
        }
    }
}

impl Drop for ClientPool {
    fn drop(&mut self) {
        if self.root_instance {
            assert!(!self.inner.is_null());
            unsafe {
                bindings::mongoc_client_pool_destroy(self.inner);
            }
        }
    }
}

pub struct SslOptions {
    inner:                bindings::mongoc_ssl_opt_t,
    // We need to store everything so both memory sticks around
    // for the C driver and we can clone this struct.
    pem_file:              Option<PathBuf>,
    _pem_file_cstring:     Option<CString>,
    pem_password:          Option<String>,
    _pem_password_cstring: Option<CString>,
    ca_file:               Option<PathBuf>,
    _ca_file_cstring:      Option<CString>,
    ca_dir:                Option<PathBuf>,
    _ca_dir_cstring:       Option<CString>,
    crl_file:              Option<PathBuf>,
    _crl_file_cstring:     Option<CString>,
    weak_cert_validation: bool
}

impl SslOptions {
    pub fn new(
        pem_file:             Option<PathBuf>,
        pem_password:         Option<String>,
        ca_file:              Option<PathBuf>,
        ca_dir:               Option<PathBuf>,
        crl_file:             Option<PathBuf>,
        weak_cert_validation: bool
    ) -> io::Result<SslOptions> {
        let pem_file_cstring     = try!(Self::cstring_from_path(&pem_file));
        let pem_password_cstring = Self::cstring_from_string(&pem_password);
        let ca_file_cstring      = try!(Self::cstring_from_path(&ca_file));
        let ca_dir_cstring       = try!(Self::cstring_from_path(&ca_dir));
        let crl_file_cstring     = try!(Self::cstring_from_path(&crl_file));

        let ssl_options = bindings::mongoc_ssl_opt_t {
            pem_file: match pem_file_cstring {
                Some(ref f) => f.as_ptr(),
                None => ptr::null()
            },
            pem_pwd: match pem_password_cstring {
                Some(ref password) => password.as_ptr(),
                None => ptr::null()
            },
            ca_file: match ca_file_cstring {
                Some(ref f) => f.as_ptr(),
                None => ptr::null()
            },
            ca_dir: match ca_dir_cstring {
                Some(ref f) => f.as_ptr(),
                None => ptr::null()
            },
            crl_file: match crl_file_cstring {
                Some(ref f) => f.as_ptr(),
                None => ptr::null()
            },
            weak_cert_validation: weak_cert_validation as u8,
            padding: unsafe { mem::uninitialized() }
        };

        Ok(SslOptions {
            inner:                 ssl_options,
            pem_file:              pem_file,
            _pem_file_cstring:     pem_file_cstring,
            pem_password:          pem_password,
            _pem_password_cstring: pem_password_cstring,
            ca_file:               ca_file,
            _ca_file_cstring:      ca_file_cstring,
            ca_dir:                ca_dir,
            _ca_dir_cstring:       ca_dir_cstring,
            crl_file:              crl_file,
            _crl_file_cstring:     crl_file_cstring,
            weak_cert_validation:  weak_cert_validation
        })
    }

    fn cstring_from_path(path: &Option<PathBuf>) -> io::Result<Option<CString>> {
        match path {
            &Some(ref p) => {
                try!(File::open(p.as_path()));
                Ok(Some(p.as_os_str().to_cstring().unwrap()))
            },
            &None => Ok(None)
        }
    }

    fn cstring_from_string(path: &Option<String>) -> Option<CString> {
        match path {
            &Some(ref p) => Some(CString::new(p.clone()).unwrap()),
            &None => None
        }
    }

    fn inner(&self) -> *const bindings::mongoc_ssl_opt_t {
        &self.inner
    }
}

impl Clone for SslOptions {
    fn clone(&self) -> SslOptions {
        SslOptions::new(
            self.pem_file.clone(),
            self.pem_password.clone(),
            self.ca_file.clone(),
            self.ca_dir.clone(),
            self.crl_file.clone(),
            self.weak_cert_validation
        ).unwrap()
    }
}

pub struct Client<'a> {
    client_pool: &'a ClientPool,
    inner:       *mut bindings::mongoc_client_t
}

impl<'a> Client<'a> {
    pub fn get_collection<DBT: Into<Vec<u8>>, CT: Into<Vec<u8>>>(&'a self, db: DBT, collection: CT) -> Collection<'a> {
        assert!(!self.inner.is_null());
        let coll = unsafe {
            let db_cstring         = CString::new(db).unwrap();
            let collection_cstring = CString::new(collection).unwrap();
            bindings::mongoc_client_get_collection(
                self.inner,
                db_cstring.as_ptr(),
                collection_cstring.as_ptr()
            )
        };
        Collection::new(CreatedBy::Client(self), coll)
    }

    pub fn get_database<S: Into<Vec<u8>>>(&'a self, db: S) -> Database<'a> {
        assert!(!self.inner.is_null());
        let coll = unsafe {
            let db_cstring = CString::new(db).unwrap();
            bindings::mongoc_client_get_database(
                self.inner,
                db_cstring.as_ptr()
            )
        };
        Database::new(self, coll)
    }

    /// Queries the server for the current server status.
    ///
    /// See: http://api.mongodb.org/c/current/mongoc_client_get_server_status.html
    pub fn get_server_status(&self, read_prefs: Option<ReadPrefs>) -> Result<Document> {
        assert!(!self.inner.is_null());

        // Bsonc to store the reply
        let mut reply = Bsonc::new();
        // Empty error that might be filled
        let mut error = BsoncError::empty();

        let success = unsafe {
            bindings::mongoc_client_get_server_status(
                self.inner,
                match read_prefs {
                    Some(ref prefs) => prefs.mut_inner(),
                    None => ptr::null_mut()
                },
                reply.mut_inner(),
                error.mut_inner()
            )
        };

        if success == 1 {
            match reply.as_document() {
                Ok(document) => return Ok(document),
                Err(error)   => return Err(error.into())
            }
        } else {
            Err(error.into())
        }
    }
}

impl<'a> Drop for Client<'a> {
    fn drop(&mut self) {
        assert!(!self.inner.is_null());
        unsafe {
            self.client_pool.push(self.inner);
        }
    }
}
