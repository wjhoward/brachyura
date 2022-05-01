from flask import Flask

# This is used to provide a backend for testing the reverse proxy
app = Flask(__name__)

@app.route('/')
def index():
    return 'This is a test backend'

app.run(host='127.0.0.1', port=8000)