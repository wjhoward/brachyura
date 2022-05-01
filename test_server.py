from flask import Flask

# This is used to provide a backend for testing the reverse proxy
app = Flask(__name__)

@app.route('/')
def index():
    return 'This is a test backend'

app.run(host='0.0.0.0', port=8000)