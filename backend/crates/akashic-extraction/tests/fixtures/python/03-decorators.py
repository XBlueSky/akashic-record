from flask import Flask
app = Flask(__name__)

@app.route('/users')
def list_users():
    return []

@app.route('/users', methods=['POST'])
def create_user():
    return {}
