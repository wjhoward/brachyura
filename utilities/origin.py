from flask import Flask, request

app = Flask(__name__)

port = 10000

@app.route("/")
def hello_world():
    http_version = request.environ.get('SERVER_PROTOCOL')
    return f"This is the Python origin server, listening on port: {port} \nrequest HTTP version: {http_version}"

if __name__ == '__main__':
    app.run(host="localhost", port=port, debug=True)
