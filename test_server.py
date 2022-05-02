from flask import Flask, make_response

# This is used to provide a backend for testing the reverse proxy
app = Flask(__name__)

@app.route("/")
def index():
    resp = make_response("This is a test backend")
    resp.headers["test-header"] = "test-value"
    return resp

app.run(host="127.0.0.1", port=8000)